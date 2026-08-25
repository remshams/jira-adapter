use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::de::DeserializeOwned;
use thiserror::Error;
use url::Url;

use crate::config::Config;
use crate::issue_key::IssueKey;
use crate::jira::issue::{Account, Issue};

const USER_AGENT: &str = concat!("jcli/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Error)]
pub enum JiraClientError {
    #[error("failed to build request URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("authentication failed (401): check JIRA_EMAIL and JIRA_API_TOKEN")]
    Unauthorized,
    #[error("forbidden (403): the account lacks permission for this operation")]
    Forbidden,
    #[error("not found (404): {0}")]
    NotFound(String),
    #[error("rate limited by Jira (429): try again later")]
    RateLimited,
    #[error("Jira API error (HTTP {status}): {body}")]
    Api { status: u16, body: String },
}

/// Blocking, GET-only client for the Jira Cloud REST API v3.
///
/// There is deliberately no method here that sends another HTTP verb; making
/// jcli writable later means consciously extending this type.
pub struct JiraClient {
    http: Client,
    base_url: Url,
    email: String,
    api_token: String,
}

impl JiraClient {
    pub fn new(config: &Config) -> Result<Self, JiraClientError> {
        let http = Client::builder().user_agent(USER_AGENT).build()?;
        Ok(JiraClient {
            http,
            base_url: config.base_url.clone(),
            email: config.email.clone(),
            api_token: config.api_token.clone(),
        })
    }

    /// Who am I? Smoke-tests authentication against `/rest/api/3/myself`.
    pub fn myself(&self) -> Result<Account, JiraClientError> {
        self.get_json("rest/api/3/myself")
    }

    pub fn issue(&self, issue_key: &IssueKey) -> Result<Issue, JiraClientError> {
        let path = format!("rest/api/3/issue/{}", issue_key.as_str());
        let mut url = self.base_url.join(&path)?;
        url.query_pairs_mut().append_pair(
            "fields",
            "summary,status,assignee,reporter,labels,description",
        );
        self.get_json_url(url, &path)
    }

    fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, JiraClientError> {
        let url = self.base_url.join(path)?;
        self.get_json_url(url, path)
    }

    fn get_json_url<T: DeserializeOwned>(
        &self,
        url: Url,
        error_path: &str,
    ) -> Result<T, JiraClientError> {
        let response = self
            .http
            .get(url)
            .basic_auth(&self.email, Some(&self.api_token))
            .send()?;

        let status = response.status();
        if status.is_success() {
            return Ok(response.json()?);
        }

        let body = sanitize_api_error_body(
            &response.text().unwrap_or_default(),
            &self.email,
            &self.api_token,
        );
        Err(match status {
            StatusCode::UNAUTHORIZED => JiraClientError::Unauthorized,
            StatusCode::FORBIDDEN => JiraClientError::Forbidden,
            StatusCode::NOT_FOUND => JiraClientError::NotFound(error_path.to_string()),
            StatusCode::TOO_MANY_REQUESTS => JiraClientError::RateLimited,
            other => JiraClientError::Api {
                status: other.as_u16(),
                body,
            },
        })
    }
}

fn sanitize_api_error_body(text: &str, email: &str, api_token: &str) -> String {
    let mut sanitized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                sanitized.push(' ');
            }
            '\n' | '\t' => sanitized.push(' '),
            character if is_unsafe_terminal_control(character) => {}
            character => sanitized.push(character),
        }
    }

    let mut redacted = sanitized;
    for secret in [api_token, email] {
        if !secret.is_empty() {
            redacted = redacted.replace(secret, "[redacted]");
        }
    }
    truncate(redacted.trim(), 500)
}

fn is_unsafe_terminal_control(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(max_chars).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::GET;
    use httpmock::MockServer;

    // base64 of "jane@example.com:secret-token"
    const AUTH_HEADER: &str = "Basic amFuZUBleGFtcGxlLmNvbTpzZWNyZXQtdG9rZW4=";

    fn client(server: &MockServer) -> JiraClient {
        JiraClient::new(&Config {
            base_url: Url::parse(&server.base_url()).unwrap(),
            email: "jane@example.com".to_string(),
            api_token: "secret-token".to_string(),
        })
        .unwrap()
    }

    #[test]
    fn myself_sends_basic_auth_and_parses_the_account() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/3/myself")
                .header("Authorization", AUTH_HEADER);
            then.status(200).json_body(serde_json::json!({
                "accountId": "5b10a2844c20165700ede21g",
                "displayName": "Jane Doe",
                "emailAddress": "jane@example.com"
            }));
        });

        let account = client(&server).myself().unwrap();

        mock.assert();
        assert_eq!(account.account_id, "5b10a2844c20165700ede21g");
        assert_eq!(account.display_name.as_deref(), Some("Jane Doe"));
        assert_eq!(account.email_address.as_deref(), Some("jane@example.com"));
    }

    #[test]
    fn issue_requests_selected_fields_and_parses_adf() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/3/issue/ABC-123")
                .query_param(
                    "fields",
                    "summary,status,assignee,reporter,labels,description",
                )
                .query_param_count(r".*", r".*", 1)
                .header("Authorization", AUTH_HEADER);
            then.status(200).json_body(serde_json::json!({
                "key": "ABC-123",
                "fields": {
                    "summary": "Fix the thing",
                    "status": {"name": "In Progress"},
                    "assignee": null,
                    "reporter": {
                        "accountId": "reporter-id",
                        "displayName": "Reporter",
                        "emailAddress": null
                    },
                    "labels": ["backend", "urgent"],
                    "description": {
                        "version": 1,
                        "type": "doc",
                        "content": [{
                            "type": "paragraph",
                            "content": [{"type": "text", "text": "Description"}]
                        }]
                    }
                }
            }));
        });

        let issue = client(&server).issue(&"ABC-123".parse().unwrap()).unwrap();

        mock.assert();
        assert_eq!(issue.key, "ABC-123");
        assert_eq!(issue.fields.summary, "Fix the thing");
        assert_eq!(issue.fields.status.name, "In Progress");
        assert!(issue.fields.assignee.is_none());
        assert_eq!(issue.fields.reporter.as_ref().unwrap().email_address, None);
        assert_eq!(issue.fields.labels, ["backend", "urgent"]);
        let description = issue.fields.description.unwrap();
        assert_eq!(description.node_type, "doc");
        assert_eq!(description.content[0].node_type, "paragraph");
        assert_eq!(
            description.content[0].content[0].text.as_deref(),
            Some("Description")
        );
    }

    #[test]
    fn appends_the_api_path_to_a_base_url_with_a_path_prefix() {
        // httpmock itself only listens at the root, so this pins down the
        // URL building directly rather than through a request.
        let mut config = Config {
            base_url: Url::parse("https://example.com/jira").unwrap(),
            email: "jane@example.com".to_string(),
            api_token: "token".to_string(),
        };
        config.base_url.set_path("/jira/");
        let client = JiraClient::new(&config).unwrap();
        let url = client.base_url.join("rest/api/3/myself").unwrap();
        assert_eq!(url.as_str(), "https://example.com/jira/rest/api/3/myself");
    }

    #[test]
    fn maps_401_to_unauthorized() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET);
            then.status(401);
        });

        let err = client(&server).myself().unwrap_err();
        assert!(matches!(err, JiraClientError::Unauthorized));
    }

    #[test]
    fn maps_403_to_forbidden() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET);
            then.status(403);
        });

        let err = client(&server).myself().unwrap_err();
        assert!(matches!(err, JiraClientError::Forbidden));
    }

    #[test]
    fn maps_404_to_not_found_with_the_request_path() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET);
            then.status(404);
        });

        let err = client(&server).myself().unwrap_err();
        match err {
            JiraClientError::NotFound(path) => assert_eq!(path, "rest/api/3/myself"),
            other => panic!("expected NotFound, got: {other}"),
        }
    }

    #[test]
    fn maps_429_to_rate_limited() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET);
            then.status(429);
        });

        let err = client(&server).myself().unwrap_err();
        assert!(matches!(err, JiraClientError::RateLimited));
    }

    #[test]
    fn maps_500_to_an_api_error_with_status_and_body() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET);
            then.status(500).body("boom");
        });

        let err = client(&server).myself().unwrap_err();
        match err {
            JiraClientError::Api { status, body } => {
                assert_eq!(status, 500);
                assert_eq!(body, "boom");
            }
            other => panic!("expected Api, got: {other}"),
        }
    }

    #[test]
    fn sanitizes_and_redacts_api_error_bodies() {
        let body = sanitize_api_error_body(
            "first line\r\nsecond\t\u{1b}[31m jane@example.com secret-token\u{202e}",
            "jane@example.com",
            "secret-token",
        );

        assert_eq!(body, "first line second [31m [redacted] [redacted]");
        assert!(!body.chars().any(is_unsafe_terminal_control));
    }

    #[test]
    fn truncate_leaves_short_text_untouched() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_cuts_long_text_by_chars_not_bytes() {
        assert_eq!(truncate("ääääää", 3), "äää...");
    }
}
