use std::io::{self, Read};

use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use reqwest::redirect::Policy;
use serde::de::DeserializeOwned;
use thiserror::Error;
use url::Url;

use crate::config::Config;
use crate::issue_key::IssueKey;
use crate::jira::attachment::{Attachment, AttachmentIssue, MAX_ATTACHMENT_ID_BYTES};
use crate::jira::issue::{Account, Issue};

const USER_AGENT: &str = concat!("jcli/", env!("CARGO_PKG_VERSION"));
const ISSUE_FIELDS: &str = "summary,status,assignee,reporter,labels,description,attachment";
pub(crate) const MAX_ATTACHMENT_METADATA_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum JiraClientError {
    #[error("failed to build request URL")]
    InvalidUrl,
    #[error("HTTP request failed")]
    Http {
        #[source]
        source: reqwest::Error,
    },
    #[error("attachment HTTP request failed")]
    AttachmentHttp {
        #[source]
        source: reqwest::Error,
    },
    #[error("Jira metadata response could not be read")]
    MetadataRead {
        #[source]
        source: io::Error,
    },
    #[error("Jira metadata response exceeds the safety limit")]
    MetadataTooLarge,
    #[error("Jira metadata response is invalid")]
    MetadataJson {
        #[source]
        source: serde_json::Error,
    },
    #[error("Jira returned an unsafe attachment ID")]
    InvalidAttachmentId,
    #[error("authentication failed (401): check JIRA_EMAIL and JIRA_API_TOKEN")]
    Unauthorized,
    #[error("forbidden (403): the account lacks permission for this operation")]
    Forbidden,
    #[error("not found (404): {0}")]
    NotFound(&'static str),
    #[error("rate limited by Jira (429): try again later")]
    RateLimited,
    #[error("Jira API error (HTTP {status})")]
    Api { status: u16 },
    #[error("attachment download failed (HTTP {status})")]
    AttachmentStatus { status: u16 },
}

impl JiraClientError {
    fn http(source: reqwest::Error) -> Self {
        Self::Http { source }
    }
}

/// Blocking, GET-only client for the Jira Cloud REST API v3.
///
/// There is deliberately no method here that sends another HTTP verb; making
/// jcli writable later means consciously extending this type.
pub struct JiraClient {
    http: Client,
    attachment_http: Client,
    base_url: Url,
    email: String,
    api_token: String,
}

impl JiraClient {
    pub fn new(config: &Config) -> Result<Self, JiraClientError> {
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .referer(false)
            .redirect(same_origin_redirect_policy())
            .build()
            .map_err(JiraClientError::http)?;
        let attachment_http = Client::builder()
            .user_agent(USER_AGENT)
            .referer(false)
            .redirect(Policy::none())
            .build()
            .map_err(JiraClientError::http)?;
        Ok(JiraClient {
            http,
            attachment_http,
            base_url: config.base_url.clone(),
            email: config.email.clone(),
            api_token: config.api_token.clone(),
        })
    }

    /// Who am I? Smoke-tests authentication against `/rest/api/3/myself`.
    pub fn myself(&self) -> Result<Account, JiraClientError> {
        let url = self.endpoint_url(&["rest", "api", "3", "myself"])?;
        self.get_json_url(url, "account")
    }

    pub fn issue(&self, issue_key: &IssueKey) -> Result<Issue, JiraClientError> {
        let mut url = self.endpoint_url(&["rest", "api", "3", "issue", issue_key.as_str()])?;
        url.query_pairs_mut().append_pair("fields", ISSUE_FIELDS);
        // The issue response now includes attachment metadata. Bound the
        // complete response so a large attachment list cannot bypass its
        // metadata limit through `issue read`.
        self.get_bounded_metadata_json_url(url, "issue")
    }

    /// Returns only the numeric issue ID (for example `"10400"`), which
    /// Xray's GraphQL API uses to reference an issue. The request asks for
    /// the harmless `summary` field only, and deserialization keeps just
    /// the top-level `id`.
    pub fn issue_id(&self, issue_key: &IssueKey) -> Result<String, JiraClientError> {
        let mut url = self.endpoint_url(&["rest", "api", "3", "issue", issue_key.as_str()])?;
        url.query_pairs_mut().append_pair("fields", "summary");
        let issue: MinimalIssue = self.get_bounded_metadata_json_url(url, "issue")?;
        Ok(issue.id)
    }

    pub fn attachments(&self, issue_key: &IssueKey) -> Result<Vec<Attachment>, JiraClientError> {
        let mut url = self.endpoint_url(&["rest", "api", "3", "issue", issue_key.as_str()])?;
        url.query_pairs_mut().append_pair("fields", "attachment");
        let issue: AttachmentIssue = self.get_bounded_metadata_json_url(url, "issue")?;
        Ok(issue.fields.attachment)
    }

    /// Returns a checked streaming response for one attachment.
    ///
    /// The attachment ID is appended as one encoded path segment. Jira-provided
    /// resource URLs are never accepted here.
    pub fn attachment_content(&self, id: &str) -> Result<Response, JiraClientError> {
        if id.is_empty() || id.len() > MAX_ATTACHMENT_ID_BYTES || matches!(id, "." | "..") {
            return Err(JiraClientError::InvalidAttachmentId);
        }
        let mut url = self.endpoint_url(&["rest", "api", "3", "attachment", "content", id])?;
        url.query_pairs_mut().append_pair("redirect", "false");

        let response = self
            .attachment_http
            .get(url)
            .basic_auth(&self.email, Some(&self.api_token))
            .send()
            .map_err(|source| JiraClientError::AttachmentHttp { source })?;
        let status = response.status();
        if status == StatusCode::OK {
            return Ok(response);
        }

        Err(match status {
            StatusCode::UNAUTHORIZED => JiraClientError::Unauthorized,
            StatusCode::FORBIDDEN => JiraClientError::Forbidden,
            StatusCode::NOT_FOUND => JiraClientError::NotFound("attachment content"),
            StatusCode::TOO_MANY_REQUESTS => JiraClientError::RateLimited,
            other => JiraClientError::AttachmentStatus {
                status: other.as_u16(),
            },
        })
    }

    fn endpoint_url(&self, segments: &[&str]) -> Result<Url, JiraClientError> {
        let mut url = self.base_url.clone();
        url.set_query(None);
        url.set_fragment(None);
        let mut path = url
            .path_segments_mut()
            .map_err(|_| JiraClientError::InvalidUrl)?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
        drop(path);
        Ok(url)
    }

    fn get_json_url<T: DeserializeOwned>(
        &self,
        url: Url,
        error_resource: &'static str,
    ) -> Result<T, JiraClientError> {
        let response = self.authenticated_get(url)?;
        let response = Self::check_json_response(response, error_resource)?;
        response.json().map_err(JiraClientError::http)
    }

    fn get_bounded_metadata_json_url<T: DeserializeOwned>(
        &self,
        url: Url,
        error_resource: &'static str,
    ) -> Result<T, JiraClientError> {
        let response = self.authenticated_get(url)?;
        let response = Self::check_json_response(response, error_resource)?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_ATTACHMENT_METADATA_BYTES as u64)
        {
            return Err(JiraClientError::MetadataTooLarge);
        }

        let body = read_bounded_metadata_body(response)?;
        serde_json::from_slice(&body).map_err(|source| JiraClientError::MetadataJson { source })
    }

    fn check_json_response(
        response: Response,
        error_resource: &'static str,
    ) -> Result<Response, JiraClientError> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        Err(match status {
            StatusCode::UNAUTHORIZED => JiraClientError::Unauthorized,
            StatusCode::FORBIDDEN => JiraClientError::Forbidden,
            StatusCode::NOT_FOUND => JiraClientError::NotFound(error_resource),
            StatusCode::TOO_MANY_REQUESTS => JiraClientError::RateLimited,
            other => JiraClientError::Api {
                status: other.as_u16(),
            },
        })
    }

    fn authenticated_get(&self, url: Url) -> Result<Response, JiraClientError> {
        self.http
            .get(url)
            .basic_auth(&self.email, Some(&self.api_token))
            .send()
            .map_err(JiraClientError::http)
    }
}

/// Minimal issue projection for lookups that only need the numeric issue ID.
#[derive(serde::Deserialize)]
struct MinimalIssue {
    id: String,
}

fn same_origin_redirect_policy() -> Policy {
    Policy::custom(|attempt| {
        let same_origin = attempt
            .previous()
            .first()
            .is_some_and(|original| urls_have_same_origin(original, attempt.url()));
        if !same_origin {
            attempt.stop()
        } else if attempt.previous().len() >= 10 {
            attempt.error("too many redirects")
        } else {
            attempt.follow()
        }
    })
}

fn urls_have_same_origin(first: &Url, second: &Url) -> bool {
    first.scheme() == second.scheme()
        && first.host_str() == second.host_str()
        && first.port_or_known_default() == second.port_or_known_default()
}

fn read_bounded_metadata_body(mut reader: impl Read) -> Result<Vec<u8>, JiraClientError> {
    let mut body = Vec::new();
    reader
        .by_ref()
        .take(MAX_ATTACHMENT_METADATA_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|source| JiraClientError::MetadataRead { source })?;
    if body.len() > MAX_ATTACHMENT_METADATA_BYTES {
        return Err(JiraClientError::MetadataTooLarge);
    }
    Ok(body)
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
    fn issue_requests_selected_fields_once_and_parses_attachment_metadata() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/3/issue/ABC-123")
                .query_param("fields", ISSUE_FIELDS)
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
                    },
                    "attachment": [{
                        "id": "opaque-id",
                        "filename": "report.pdf",
                        "size": 42,
                        "content": "https://ignored.example/content"
                    }]
                }
            }));
        });

        let issue = client(&server).issue(&"ABC-123".parse().unwrap()).unwrap();

        mock.assert_calls(1);
        assert_eq!(issue.key, "ABC-123");
        assert_eq!(issue.fields.summary, "Fix the thing");
        assert_eq!(issue.fields.status.name, "In Progress");
        assert!(issue.fields.assignee.is_none());
        assert_eq!(issue.fields.reporter.as_ref().unwrap().email_address, None);
        assert_eq!(issue.fields.labels, ["backend", "urgent"]);
        assert_eq!(issue.fields.attachment.len(), 1);
        assert_eq!(issue.fields.attachment[0].id, "opaque-id");
        let description = issue.fields.description.unwrap();
        assert_eq!(description.node_type, "doc");
        assert_eq!(description.content[0].node_type, "paragraph");
        assert_eq!(
            description.content[0].content[0].text.as_deref(),
            Some("Description")
        );
    }

    #[test]
    fn issue_id_requests_only_the_summary_field_and_reads_only_the_top_level_id() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/3/issue/ABC-123")
                .query_param("fields", "summary")
                .query_param_count(r".*", r".*", 1)
                .header("Authorization", AUTH_HEADER);
            then.status(200).json_body(serde_json::json!({
                "id": "10400",
                "key": "ABC-123",
                "fields": {
                    "summary": "Whatever",
                    "status": {"name": "In Progress"},
                    "description": {"type": "doc", "content": []}
                }
            }));
        });

        let issue_id = client(&server)
            .issue_id(&"ABC-123".parse().unwrap())
            .unwrap();

        mock.assert_calls(1);
        assert_eq!(issue_id, "10400");
    }

    #[test]
    fn attachment_metadata_uses_only_the_attachment_field() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/3/issue/ABC-123")
                .query_param("fields", "attachment")
                .query_param_count(r".*", r".*", 1)
                .header("Authorization", AUTH_HEADER);
            then.status(200).json_body(serde_json::json!({
                "fields": {"attachment": [{
                    "id": "a/b?c#d",
                    "filename": "report.pdf",
                    "size": 4,
                    "content": "https://ignored.example/content",
                    "self": "https://ignored.example/self",
                    "thumbnail": "https://ignored.example/thumbnail"
                }]}
            }));
        });

        let attachments = client(&server)
            .attachments(&"ABC-123".parse().unwrap())
            .unwrap();

        mock.assert_calls(1);
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].id, "a/b?c#d");
    }

    #[test]
    fn attachment_metadata_response_is_bounded_without_truncation() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET).path("/rest/api/3/issue/ABC-123");
            then.status(200)
                .body(vec![b' '; MAX_ATTACHMENT_METADATA_BYTES + 1]);
        });

        let error = client(&server)
            .attachments(&"ABC-123".parse().unwrap())
            .unwrap_err();

        mock.assert_calls(1);
        assert!(matches!(error, JiraClientError::MetadataTooLarge));
    }

    #[test]
    fn metadata_reader_rejects_an_unannounced_oversized_body_without_truncation() {
        let error = read_bounded_metadata_body(std::io::Cursor::new(vec![
            b' ';
            MAX_ATTACHMENT_METADATA_BYTES
                + 1
        ]))
        .unwrap_err();

        assert!(matches!(error, JiraClientError::MetadataTooLarge));
    }

    #[test]
    fn issue_response_with_attachment_metadata_is_bounded() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET).path("/rest/api/3/issue/ABC-123");
            then.status(200)
                .body(vec![b' '; MAX_ATTACHMENT_METADATA_BYTES + 1]);
        });

        let error = client(&server)
            .issue(&"ABC-123".parse().unwrap())
            .unwrap_err();

        mock.assert_calls(1);
        assert!(matches!(error, JiraClientError::MetadataTooLarge));
    }

    #[test]
    fn attachment_content_encodes_the_id_as_one_segment_and_has_one_query() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/3/attachment/content/a%2Fb%3Fc%23d")
                .query_param("redirect", "false")
                .query_param_count(r".*", r".*", 1)
                .header("Authorization", AUTH_HEADER)
                .header_missing("Range");
            then.status(200).body("data");
        });

        let response = client(&server).attachment_content("a/b?c#d").unwrap();

        mock.assert_calls(1);
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn dot_attachment_ids_are_rejected_before_a_request() {
        let server = MockServer::start();
        let any_request = server.mock(|when, then| {
            when.method(GET);
            then.status(200);
        });
        let client = client(&server);

        for id in [".", ".."] {
            let error = client.attachment_content(id).unwrap_err();
            assert!(matches!(error, JiraClientError::InvalidAttachmentId));
            assert!(!error.to_string().contains(id));
        }
        let oversized_id = "a".repeat(MAX_ATTACHMENT_ID_BYTES + 1);
        assert!(matches!(
            client.attachment_content(&oversized_id).unwrap_err(),
            JiraClientError::InvalidAttachmentId
        ));
        any_request.assert_calls(0);
    }

    #[test]
    fn attachment_content_rejects_partial_content_without_a_range_request() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/3/attachment/content/id")
                .query_param("redirect", "false");
            then.status(206).body("partial");
        });

        let error = client(&server).attachment_content("id").unwrap_err();

        mock.assert_calls(1);
        assert!(matches!(
            error,
            JiraClientError::AttachmentStatus { status: 206 }
        ));
    }

    #[test]
    fn json_endpoints_follow_redirects() {
        let server = MockServer::start();
        let myself_redirect = server.mock(|when, then| {
            when.method(GET).path("/rest/api/3/myself");
            then.status(302).header("Location", "/redirected-myself");
        });
        let myself_target = server.mock(|when, then| {
            when.method(GET).path("/redirected-myself");
            then.status(200).json_body(serde_json::json!({
                "accountId": "account-id",
                "displayName": "Jane"
            }));
        });
        let issue_redirect = server.mock(|when, then| {
            when.method(GET).path("/rest/api/3/issue/ABC-123");
            then.status(302).header("Location", "/redirected-issue");
        });
        let issue_target = server.mock(|when, then| {
            when.method(GET).path("/redirected-issue");
            then.status(200).json_body(serde_json::json!({
                "key": "ABC-123",
                "fields": {
                    "summary": "Summary",
                    "status": {"name": "Open"},
                    "assignee": null,
                    "reporter": null,
                    "labels": [],
                    "description": null,
                    "attachment": []
                }
            }));
        });
        let metadata_redirect = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/3/issue/XYZ-789")
                .query_param("fields", "attachment");
            then.status(302).header("Location", "/redirected-metadata");
        });
        let metadata_target = server.mock(|when, then| {
            when.method(GET).path("/redirected-metadata");
            then.status(200)
                .json_body(serde_json::json!({"fields": {"attachment": []}}));
        });
        let client = client(&server);

        client.myself().unwrap();
        client.issue(&"ABC-123".parse().unwrap()).unwrap();
        client.attachments(&"XYZ-789".parse().unwrap()).unwrap();

        for mock in [
            myself_redirect,
            myself_target,
            issue_redirect,
            issue_target,
            metadata_redirect,
            metadata_target,
        ] {
            mock.assert_calls(1);
        }
    }

    #[test]
    fn json_endpoints_reject_cross_origin_redirects() {
        let source = MockServer::start();
        let target = MockServer::start();
        let redirect = source.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/3/myself")
                .header("Authorization", AUTH_HEADER);
            then.status(302)
                .header("Location", format!("{}/target", target.base_url()));
        });
        let redirected_target = target.mock(|when, then| {
            when.method(GET).path("/target");
            then.status(200).json_body(serde_json::json!({
                "accountId": "should-not-be-read"
            }));
        });

        let error = client(&source).myself().unwrap_err();

        redirect.assert_calls(1);
        redirected_target.assert_calls(0);
        assert!(matches!(error, JiraClientError::Api { status: 302 }));
    }

    #[test]
    fn content_redirects_are_not_followed_and_do_not_forward_auth() {
        let server = MockServer::start();
        let redirect = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/3/attachment/content/id")
                .query_param("redirect", "false")
                .header("Authorization", AUTH_HEADER);
            then.status(302).header("Location", "/untrusted-target");
        });
        let target = server.mock(|when, then| {
            when.method(GET).path("/untrusted-target");
            then.status(200).body("should not be read");
        });

        let error = client(&server).attachment_content("id").unwrap_err();

        redirect.assert_calls(1);
        target.assert_calls(0);
        assert!(matches!(
            error,
            JiraClientError::AttachmentStatus { status: 302 }
        ));
    }

    #[test]
    fn appends_api_segments_to_a_base_url_with_a_path_prefix() {
        let mut config = Config {
            base_url: Url::parse("https://example.com/jira").unwrap(),
            email: "jane@example.com".to_string(),
            api_token: "token".to_string(),
        };
        config.base_url.set_path("/jira/");
        let client = JiraClient::new(&config).unwrap();
        let url = client
            .endpoint_url(&["rest", "api", "3", "attachment", "content", "a/b"])
            .unwrap();
        assert_eq!(
            url.as_str(),
            "https://example.com/jira/rest/api/3/attachment/content/a%2Fb"
        );
    }

    #[test]
    fn endpoint_builder_discards_base_query_and_fragment() {
        let config = Config {
            base_url: Url::parse("https://example.com/jira/?unsafe=1#fragment").unwrap(),
            email: "jane@example.com".to_string(),
            api_token: "token".to_string(),
        };
        let client = JiraClient::new(&config).unwrap();
        let url = client
            .endpoint_url(&["rest", "api", "3", "myself"])
            .unwrap();

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
    fn maps_404_to_not_found_with_a_safe_resource_name() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET);
            then.status(404);
        });

        let err = client(&server).myself().unwrap_err();
        match err {
            JiraClientError::NotFound(resource) => assert_eq!(resource, "account"),
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
    fn maps_500_to_a_status_only_api_error() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET);
            then.status(500)
                .body("secret-token\n\u{1b}[31m untrusted response data");
        });

        let error = client(&server).myself().unwrap_err();

        assert!(matches!(error, JiraClientError::Api { status: 500 }));
        assert_eq!(error.to_string(), "Jira API error (HTTP 500)");
    }

    #[test]
    fn attachment_status_errors_do_not_echo_response_data() {
        let server = MockServer::start();
        let secret_body = "https://content.example/private\n\u{1b}[31mraw metadata";
        server.mock(|when, then| {
            when.method(GET);
            then.status(500).body(secret_body);
        });

        let error = client(&server).attachment_content("unsafe/id").unwrap_err();
        let rendered = error.to_string();

        assert_eq!(rendered, "attachment download failed (HTTP 500)");
        assert!(!rendered.contains("content.example"));
        assert!(!rendered.contains("unsafe/id"));
        assert!(!rendered.contains('\u{1b}'));
    }
}
