use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use thiserror::Error;
use url::Url;

use crate::config::Config;

const USER_AGENT: &str = concat!("jcli/", env!("CARGO_PKG_VERSION"));

/// A Jira Cloud account, as returned by `/rest/api/3/myself` and embedded in
/// issue fields such as assignee and reporter.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub account_id: String,
    pub display_name: Option<String>,
    pub email_address: Option<String>,
}

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

    fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, JiraClientError> {
        let url = self.base_url.join(path)?;
        let response = self
            .http
            .get(url)
            .basic_auth(&self.email, Some(&self.api_token))
            .send()?;

        let status = response.status();
        if status.is_success() {
            return Ok(response.json()?);
        }

        let body = truncate(&response.text().unwrap_or_default(), 500);
        Err(match status {
            StatusCode::UNAUTHORIZED => JiraClientError::Unauthorized,
            StatusCode::FORBIDDEN => JiraClientError::Forbidden,
            StatusCode::NOT_FOUND => JiraClientError::NotFound(path.to_string()),
            StatusCode::TOO_MANY_REQUESTS => JiraClientError::RateLimited,
            other => JiraClientError::Api {
                status: other.as_u16(),
                body,
            },
        })
    }
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
    fn truncate_leaves_short_text_untouched() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_cuts_long_text_by_chars_not_bytes() {
        assert_eq!(truncate("ääääää", 3), "äää...");
    }
}
