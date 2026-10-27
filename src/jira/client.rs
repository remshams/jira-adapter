use std::collections::BTreeMap;
use std::io::{self, Read};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use reqwest::redirect::Policy;
use serde::de::DeserializeOwned;
use thiserror::Error;
use url::Url;

use crate::config::Config;
use crate::issue_key::IssueKey;
use crate::jira::attachment::{Attachment, AttachmentIssue, MAX_ATTACHMENT_ID_BYTES};
use crate::jira::issue::{Account, Issue, IssueReference, IssueType};
use crate::jira::search::{SearchPage, SearchResults};
use crate::output::terminal_field;

const USER_AGENT: &str = concat!("jcli/", env!("CARGO_PKG_VERSION"));
const ISSUE_FIELDS: &str =
    "summary,issuetype,status,assignee,reporter,labels,created,description,attachment";
const SEARCH_FIELDS: &str = "summary,status,issuetype,created,updated,labels";
/// The most issues jcli asks Jira for in one search request.
const SEARCH_PAGE_SIZE: usize = 100;
pub(crate) const MAX_ATTACHMENT_METADATA_BYTES: usize = 16 * 1024 * 1024;
/// Jira error bodies are short JSON documents; anything larger is not one.
const MAX_ERROR_RESPONSE_BYTES: usize = 64 * 1024;
/// Stops a connection attempt to an unreachable Jira endpoint early.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// reqwest's blocking client applies this timeout to sending the request and
/// receiving the response headers, and then separately to each read call on
/// a streamed body. It limits idle time, not total time: an attachment
/// download keeps going as long as data keeps arriving, while a server that
/// sends nothing for this long fails the request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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
    #[error("invalid JQL query (400): {0}")]
    InvalidJql(String),
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
        Self::build(config, CONNECT_TIMEOUT, REQUEST_TIMEOUT)
    }

    #[cfg(test)]
    fn with_timeouts(
        config: &Config,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Self, JiraClientError> {
        Self::build(config, connect_timeout, request_timeout)
    }

    fn build(
        config: &Config,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Self, JiraClientError> {
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .referer(false)
            .redirect(same_origin_redirect_policy())
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .build()
            .map_err(JiraClientError::http)?;
        let attachment_http = Client::builder()
            .user_agent(USER_AGENT)
            .referer(false)
            .redirect(Policy::none())
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
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

    /// Returns the numeric issue ID (for example `"10400"`) and the issue
    /// type name in one request. The request asks for the `issuetype` field
    /// only, and deserialization keeps just the top-level `id` and the type
    /// name.
    pub fn issue_reference(&self, issue_key: &IssueKey) -> Result<IssueReference, JiraClientError> {
        let mut url = self.endpoint_url(&["rest", "api", "3", "issue", issue_key.as_str()])?;
        url.query_pairs_mut().append_pair("fields", "issuetype");
        let issue: MinimalIssue = self.get_bounded_metadata_json_url(url, "issue")?;
        let issue_type = issue
            .fields
            .and_then(|fields| fields.issue_type)
            .and_then(|issue_type| issue_type.name);
        Ok(IssueReference {
            id: issue.id,
            issue_type,
        })
    }

    /// Runs a JQL search and returns at most `limit` issues in Jira's order.
    /// Each request asks for up to 100 issues and passes the previous page's
    /// `nextPageToken`. Paging stops once `limit` issues are collected, when
    /// a page is the last one, has no token, or adds no issues; the last
    /// rule ends a server that keeps handing out tokens for empty pages. The
    /// query is passed to Jira unchanged; a failed page fails the whole search.
    pub fn search_issues(&self, jql: &str, limit: u32) -> Result<SearchResults, JiraClientError> {
        let limit = limit as usize;
        let mut issues = Vec::new();
        let mut token = None;
        loop {
            let max_results = (limit - issues.len()).min(SEARCH_PAGE_SIZE);
            let page = self.search_page(jql, max_results, token.as_deref())?;
            let last = is_final_page(&page);
            issues.extend(page.issues.unwrap_or_default());
            token = page.next_page_token;
            if last || issues.len() >= limit {
                return Ok(SearchResults {
                    issues,
                    is_last: page.is_last,
                });
            }
        }
    }

    /// Fetches one search page. A 400 response carries Jira's explanation of
    /// the rejected query; see [`invalid_query_error`].
    fn search_page(
        &self,
        jql: &str,
        max_results: usize,
        token: Option<&str>,
    ) -> Result<SearchPage, JiraClientError> {
        let mut url = self.endpoint_url(&["rest", "api", "3", "search", "jql"])?;
        let mut query = url.query_pairs_mut();
        query
            .append_pair("jql", jql)
            .append_pair("maxResults", &max_results.to_string())
            .append_pair("fields", SEARCH_FIELDS);
        if let Some(token) = token {
            query.append_pair("nextPageToken", token);
        }
        drop(query);
        let response = self.authenticated_get(url)?;
        if response.status() == StatusCode::BAD_REQUEST {
            return Err(invalid_query_error(response, &self.credential_values()));
        }
        Self::bounded_metadata_json(response, "issue search")
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
        validate_attachment_id(id)?;
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
        Err(status_error(status, "attachment content", |status| {
            JiraClientError::AttachmentStatus { status }
        }))
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
        Self::bounded_metadata_json(response, error_resource)
    }

    fn bounded_metadata_json<T: DeserializeOwned>(
        response: Response,
        error_resource: &'static str,
    ) -> Result<T, JiraClientError> {
        let response = Self::check_json_response(response, error_resource)?;
        if content_length_exceeds(response.content_length(), MAX_ATTACHMENT_METADATA_BYTES) {
            return Err(JiraClientError::MetadataTooLarge);
        }

        let body = read_bounded_body(response, MAX_ATTACHMENT_METADATA_BYTES)?;
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
        Err(status_error(status, error_resource, |status| {
            JiraClientError::Api { status }
        }))
    }

    /// The request credentials as they could appear in a reflected error
    /// body: the Basic authorization value, its base64 part, and the API
    /// token. The full header value comes first so it is redacted whole.
    /// The email alone is not a secret and legitimately appears in JQL.
    fn credential_values(&self) -> [String; 3] {
        let encoded = BASE64_STANDARD.encode(format!("{}:{}", self.email, self.api_token));
        [format!("Basic {encoded}"), encoded, self.api_token.clone()]
    }

    fn authenticated_get(&self, url: Url) -> Result<Response, JiraClientError> {
        self.http
            .get(url)
            .basic_auth(&self.email, Some(&self.api_token))
            .send()
            .map_err(JiraClientError::http)
    }
}

fn validate_attachment_id(id: &str) -> Result<(), JiraClientError> {
    if id.is_empty() || id.len() > MAX_ATTACHMENT_ID_BYTES || matches!(id, "." | "..") {
        Err(JiraClientError::InvalidAttachmentId)
    } else {
        Ok(())
    }
}

/// Maps the failure statuses shared by all Jira endpoints. `NotFound` names
/// `resource`, and any other status becomes the error built by `other`.
fn status_error(
    status: StatusCode,
    resource: &'static str,
    other: fn(u16) -> JiraClientError,
) -> JiraClientError {
    match status {
        StatusCode::UNAUTHORIZED => JiraClientError::Unauthorized,
        StatusCode::FORBIDDEN => JiraClientError::Forbidden,
        StatusCode::NOT_FOUND => JiraClientError::NotFound(resource),
        StatusCode::TOO_MANY_REQUESTS => JiraClientError::RateLimited,
        status => other(status.as_u16()),
    }
}

/// Minimal issue projection for lookups that need the numeric issue ID and
/// the issue type. Jira may omit `fields` or send a null `issuetype`.
#[derive(serde::Deserialize)]
struct MinimalIssue {
    id: String,
    fields: Option<MinimalIssueFields>,
}

#[derive(serde::Deserialize)]
struct MinimalIssueFields {
    #[serde(rename = "issuetype")]
    issue_type: Option<IssueType>,
}

/// Jira's error body: general messages plus messages keyed by field.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ErrorCollection {
    error_messages: Option<Vec<String>>,
    errors: Option<BTreeMap<String, String>>,
}

/// Maps a 400 from the search endpoint. Jira explains a rejected JQL query
/// in the error body; its messages are joined into one terminal-safe line
/// so an agent can fix the query. Occurrences of `credentials` in field
/// names and messages become `[REDACTED]`. A body that is too large,
/// unreadable, not an error collection, or without messages falls back to
/// the generic status error.
/// Whether paging ends after `page`: Jira marked it the last one, gave no
/// token for the next one, or sent no issues on it.
fn is_final_page(page: &SearchPage) -> bool {
    page.is_last == Some(true)
        || page.next_page_token.is_none()
        || page.issues.as_ref().is_none_or(Vec::is_empty)
}

fn invalid_query_error(response: Response, credentials: &[String]) -> JiraClientError {
    query_error_messages(response, credentials).map_or(
        JiraClientError::Api { status: 400 },
        JiraClientError::InvalidJql,
    )
}

fn query_error_messages(response: Response, credentials: &[String]) -> Option<String> {
    if content_length_exceeds(response.content_length(), MAX_ERROR_RESPONSE_BYTES) {
        return None;
    }
    let body = read_bounded_body(response, MAX_ERROR_RESPONSE_BYTES).ok()?;
    let collection: ErrorCollection = serde_json::from_slice(&body).ok()?;
    let field_messages = collection
        .errors
        .into_iter()
        .flatten()
        .filter_map(|(field, message)| {
            let message = error_line(&message, credentials)?;
            let field = error_line(&field, credentials).unwrap_or_default();
            Some(format!("{field}: {message}"))
        });
    let messages: Vec<String> = collection
        .error_messages
        .into_iter()
        .flatten()
        .filter_map(|message| error_line(&message, credentials))
        .chain(field_messages)
        .collect();
    (!messages.is_empty()).then(|| messages.join("; "))
}

/// One terminal-safe piece of a Jira error with `credentials` redacted, or
/// `None` when nothing visible remains. Redaction runs after sanitizing so
/// control characters inside a credential cannot hide it.
fn error_line(text: &str, credentials: &[String]) -> Option<String> {
    let line = terminal_field(Some(text), "").trim().to_owned();
    let line = credentials.iter().fold(line, |line, credential| {
        line.replace(credential.as_str(), "[REDACTED]")
    });
    (!line.is_empty()).then_some(line)
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

fn content_length_exceeds(content_length: Option<u64>, limit: usize) -> bool {
    content_length.is_some_and(|length| length > limit as u64)
}

fn read_bounded_body(mut reader: impl Read, limit: usize) -> Result<Vec<u8>, JiraClientError> {
    let mut body = Vec::new();
    reader
        .by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|source| JiraClientError::MetadataRead { source })?;
    if body.len() > limit {
        return Err(JiraClientError::MetadataTooLarge);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::GET;
    use httpmock::MockServer;
    use std::io::Write;
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::thread;
    use std::time::Instant;

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
    fn metadata_limit_matches_the_response_contract() {
        assert_eq!(MAX_ATTACHMENT_METADATA_BYTES, 16_777_216);
    }

    #[test]
    fn content_length_limit_accepts_the_boundary() {
        assert!(!content_length_exceeds(None, 20));
        assert!(!content_length_exceeds(Some(20), 20));
        assert!(content_length_exceeds(Some(21), 20));
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
                    "issuetype": {"name": "Bug"},
                    "status": {"name": "In Progress"},
                    "assignee": null,
                    "reporter": {
                        "accountId": "reporter-id",
                        "displayName": "Reporter",
                        "emailAddress": null
                    },
                    "labels": ["backend", "urgent"],
                    "created": "2026-10-17T02:13:45.123+0200",
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
        assert_eq!(
            issue.fields.issue_type.unwrap().name.as_deref(),
            Some("Bug")
        );
        assert_eq!(issue.fields.status.name, "In Progress");
        assert!(issue.fields.assignee.is_none());
        assert_eq!(issue.fields.reporter.as_ref().unwrap().email_address, None);
        assert_eq!(issue.fields.labels, ["backend", "urgent"]);
        assert_eq!(
            issue.fields.created.as_deref(),
            Some("2026-10-17T02:13:45.123+0200")
        );
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

    fn issue_reference_mock(server: &MockServer, body: serde_json::Value) -> httpmock::Mock<'_> {
        server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/3/issue/ABC-123")
                .query_param("fields", "issuetype")
                .query_param_count(r".*", r".*", 1)
                .header("Authorization", AUTH_HEADER);
            then.status(200).json_body(body);
        })
    }

    fn issue_reference(server: &MockServer) -> IssueReference {
        client(server)
            .issue_reference(&"ABC-123".parse().unwrap())
            .unwrap()
    }

    #[test]
    fn issue_reference_requests_only_the_issuetype_field_and_reads_id_and_type_name() {
        let server = MockServer::start();
        let mock = issue_reference_mock(
            &server,
            serde_json::json!({
                "id": "10400",
                "key": "ABC-123",
                "fields": {
                    "issuetype": {"id": "10010", "name": "Test Execution", "subtask": false},
                    "summary": "Whatever"
                }
            }),
        );

        let reference = issue_reference(&server);

        mock.assert_calls(1);
        assert_eq!(
            reference,
            IssueReference {
                id: "10400".to_owned(),
                issue_type: Some("Test Execution".to_owned()),
            }
        );
    }

    #[test]
    fn issue_reference_keeps_a_missing_issue_type_name_as_none() {
        for body in [
            serde_json::json!({"id": "10400"}),
            serde_json::json!({"id": "10400", "fields": null}),
            serde_json::json!({"id": "10400", "fields": {}}),
            serde_json::json!({"id": "10400", "fields": {"issuetype": null}}),
            serde_json::json!({"id": "10400", "fields": {"issuetype": {"name": null}}}),
        ] {
            let server = MockServer::start();
            let mock = issue_reference_mock(&server, body.clone());

            let reference = issue_reference(&server);

            mock.assert_calls(1);
            assert_eq!(reference.id, "10400", "{body}");
            assert_eq!(reference.issue_type, None, "{body}");
        }
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
    fn metadata_reader_enforces_the_exact_boundary_without_truncation() {
        let exact = read_bounded_body(
            std::io::Cursor::new(vec![b' '; MAX_ATTACHMENT_METADATA_BYTES]),
            MAX_ATTACHMENT_METADATA_BYTES,
        )
        .unwrap();
        assert_eq!(exact.len(), 16_777_216);

        let error = read_bounded_body(
            std::io::Cursor::new(vec![b' '; MAX_ATTACHMENT_METADATA_BYTES + 1]),
            MAX_ATTACHMENT_METADATA_BYTES,
        )
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
    fn attachment_ids_enforce_exact_boundaries_before_a_request() {
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

        let maximum_id = "a".repeat(256);
        assert_eq!(
            client.attachment_content(&maximum_id).unwrap().status(),
            StatusCode::OK
        );
        any_request.assert_calls(1);
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

    /// Starts a raw TCP server for one connection. It reads the request and
    /// hands the stream to `respond`. Callers that need to stall keep the
    /// thread detached so the test finishes through the client timeout.
    fn raw_server(respond: impl FnOnce(TcpStream) + Send + 'static) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            respond(stream);
        });
        addr
    }

    fn stalled_server() -> SocketAddr {
        raw_server(|_stream| thread::sleep(Duration::from_secs(10)))
    }

    fn short_timeout_client(addr: SocketAddr, request_timeout: Duration) -> JiraClient {
        JiraClient::with_timeouts(
            &Config {
                base_url: Url::parse(&format!("http://{addr}/")).unwrap(),
                email: "jane@example.com".to_string(),
                api_token: "secret-token".to_string(),
            },
            Duration::from_millis(50),
            request_timeout,
        )
        .unwrap()
    }

    fn assert_io_timeout(error: &io::Error) {
        let source = error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<reqwest::Error>())
            .expect("the read error should wrap a reqwest error");
        assert!(source.is_timeout(), "{source:?}");
    }

    #[test]
    fn timeouts_match_the_documented_values() {
        assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(10));
        assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn a_stalled_json_response_times_out() {
        let client = short_timeout_client(stalled_server(), Duration::from_millis(400));

        let started = Instant::now();
        let error = client.myself().unwrap_err();

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the stalled response took {:?} to fail",
            started.elapsed()
        );
        match error {
            JiraClientError::Http { source } => assert!(source.is_timeout(), "{source:?}"),
            other => panic!("expected a timeout, got {other:?}"),
        }
    }

    #[test]
    fn a_stalled_metadata_body_times_out() {
        let addr = raw_server(|mut stream| {
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\n\r\n{\"fields\":")
                .unwrap();
            thread::sleep(Duration::from_secs(10));
        });
        let client = short_timeout_client(addr, Duration::from_millis(400));

        let started = Instant::now();
        let error = client.attachments(&"ABC-123".parse().unwrap()).unwrap_err();

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the stalled body took {:?} to fail",
            started.elapsed()
        );
        match error {
            JiraClientError::MetadataRead { source } => assert_io_timeout(&source),
            other => panic!("expected a timeout, got {other:?}"),
        }
    }

    #[test]
    fn a_stalled_attachment_response_times_out() {
        let client = short_timeout_client(stalled_server(), Duration::from_millis(400));

        let started = Instant::now();
        let error = client.attachment_content("id").unwrap_err();

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the stalled response took {:?} to fail",
            started.elapsed()
        );
        match error {
            JiraClientError::AttachmentHttp { source } => {
                assert!(source.is_timeout(), "{source:?}");
            }
            other => panic!("expected a timeout, got {other:?}"),
        }
    }

    #[test]
    fn a_stalled_attachment_body_times_out() {
        let addr = raw_server(|mut stream| {
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nabcd")
                .unwrap();
            thread::sleep(Duration::from_secs(10));
        });
        let client = short_timeout_client(addr, Duration::from_millis(400));
        let mut response = client.attachment_content("id").unwrap();

        let started = Instant::now();
        let mut body = Vec::new();
        let error = response.read_to_end(&mut body).unwrap_err();

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the stalled body took {:?} to fail",
            started.elapsed()
        );
        assert_eq!(body, b"abcd");
        assert_io_timeout(&error);
    }

    #[test]
    fn attachment_downloads_may_outlast_the_request_timeout_while_data_arrives() {
        // Eight one-byte chunks 150 ms apart take well over the 500 ms
        // timeout in total, but no single read waits that long.
        let request_timeout = Duration::from_millis(500);
        let addr = raw_server(|mut stream| {
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n")
                .unwrap();
            for byte in b"abcdefgh" {
                thread::sleep(Duration::from_millis(150));
                stream.write_all(&[*byte]).unwrap();
                stream.flush().unwrap();
            }
        });
        let client = short_timeout_client(addr, request_timeout);

        let started = Instant::now();
        let mut response = client.attachment_content("id").unwrap();
        let mut body = Vec::new();
        response.read_to_end(&mut body).unwrap();

        assert_eq!(body, b"abcdefgh");
        assert!(
            started.elapsed() > request_timeout,
            "the download finished in {:?}, so it did not test the total time",
            started.elapsed()
        );
    }

    const SEARCH_JQL: &str = r#"project = "ABC" AND summary ~ "a&b=c+d #1" ORDER BY created DESC"#;

    fn search_mock<'a>(
        server: &'a MockServer,
        status: u16,
        body: impl Into<String>,
    ) -> httpmock::Mock<'a> {
        let body = body.into();
        server.mock(move |when, then| {
            when.method(GET)
                .path("/rest/api/3/search/jql")
                .query_param("jql", SEARCH_JQL)
                .query_param("maxResults", "25")
                .query_param("fields", "summary,status,issuetype,created,updated,labels")
                .query_param_count(r".*", r".*", 3)
                .header("Authorization", AUTH_HEADER);
            then.status(status)
                .header("Content-Type", "application/json")
                .body(body);
        })
    }

    fn search(server: &MockServer) -> Result<SearchResults, JiraClientError> {
        search_with_limit(server, 25)
    }

    fn search_with_limit(
        server: &MockServer,
        limit: u32,
    ) -> Result<SearchResults, JiraClientError> {
        client(server).search_issues(SEARCH_JQL, limit)
    }

    fn search_keys(results: &SearchResults) -> Vec<&str> {
        results
            .issues
            .iter()
            .map(|issue| issue.key.as_str())
            .collect()
    }

    /// Issues `ABC-<first>` to `ABC-<last>` as Jira search hits.
    fn search_hits(first: usize, last: usize) -> Vec<serde_json::Value> {
        (first..=last)
            .map(|number| serde_json::json!({"key": format!("ABC-{number}")}))
            .collect()
    }

    fn expected_keys(first: usize, last: usize) -> Vec<String> {
        (first..=last)
            .map(|number| format!("ABC-{number}"))
            .collect()
    }

    /// Mocks one search page. The first page sends no `nextPageToken`; later
    /// pages send the given token. Every other parameter stays the same.
    fn search_page_mock<'a>(
        server: &'a MockServer,
        max_results: usize,
        token: Option<&str>,
        status: u16,
        body: serde_json::Value,
    ) -> httpmock::Mock<'a> {
        let token = token.map(str::to_owned);
        server.mock(move |when, then| {
            let when = when
                .method(GET)
                .path("/rest/api/3/search/jql")
                .query_param("jql", SEARCH_JQL)
                .query_param("maxResults", max_results.to_string())
                .query_param("fields", "summary,status,issuetype,created,updated,labels")
                .header("Authorization", AUTH_HEADER);
            match token {
                Some(token) => when
                    .query_param("nextPageToken", token)
                    .query_param_count(r".*", r".*", 4),
                None => when.query_param_count(r".*", r".*", 3),
            };
            then.status(status).json_body(body);
        })
    }

    /// A JSON error collection padded with trailing whitespace to exactly
    /// `length` bytes.
    fn padded_error_body(length: usize) -> String {
        let mut body = serde_json::json!({"errorMessages": ["Bad query"]}).to_string();
        body.push_str(&" ".repeat(length - body.len()));
        body
    }

    #[test]
    fn error_body_limit_matches_the_response_contract() {
        assert_eq!(MAX_ERROR_RESPONSE_BYTES, 65_536);
    }

    #[test]
    fn search_sends_the_encoded_jql_limit_and_fields_and_keeps_jira_order() {
        let server = MockServer::start();
        let mock = search_mock(
            &server,
            200,
            serde_json::json!({
                "issues": [
                    {"id": "2", "key": "ABC-2", "fields": {"summary": "Second"}},
                    {"id": "1", "key": "ABC-1", "fields": {"summary": "First"}}
                ],
                "isLast": true
            })
            .to_string(),
        );

        let results = search(&server).unwrap();

        mock.assert_calls(1);
        assert_eq!(results.is_last, Some(true));
        assert_eq!(search_keys(&results), ["ABC-2", "ABC-1"]);
    }

    #[test]
    fn search_asks_for_the_whole_limit_up_to_100_in_one_request() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/3/search/jql")
                .query_param("jql", "")
                .query_param("maxResults", "100");
            then.status(200)
                .json_body(serde_json::json!({"issues": [], "isLast": true}));
        });

        let results = client(&server).search_issues("", 100).unwrap();

        mock.assert_calls(1);
        assert_eq!(results.is_last, Some(true));
        assert!(results.issues.is_empty());
    }

    #[test]
    fn search_pages_until_the_limit_and_asks_only_for_the_remainder() {
        let server = MockServer::start();
        let first = search_page_mock(
            &server,
            100,
            None,
            200,
            serde_json::json!({"issues": search_hits(1, 100), "nextPageToken": "t1", "isLast": false}),
        );
        let second = search_page_mock(
            &server,
            100,
            Some("t1"),
            200,
            serde_json::json!({"issues": search_hits(101, 200), "nextPageToken": "t2", "isLast": false}),
        );
        let third = search_page_mock(
            &server,
            50,
            Some("t2"),
            200,
            serde_json::json!({"issues": search_hits(201, 250), "nextPageToken": "t3", "isLast": false}),
        );

        let results = search_with_limit(&server, 250).unwrap();

        first.assert_calls(1);
        second.assert_calls(1);
        third.assert_calls(1);
        assert_eq!(search_keys(&results), expected_keys(1, 250));
        assert_eq!(results.is_last, Some(false));
    }

    #[test]
    fn search_stops_at_the_last_page_before_the_limit() {
        let server = MockServer::start();
        let first = search_page_mock(
            &server,
            100,
            None,
            200,
            serde_json::json!({"issues": search_hits(1, 100), "nextPageToken": "t1", "isLast": false}),
        );
        let second = search_page_mock(
            &server,
            100,
            Some("t1"),
            200,
            serde_json::json!({"issues": search_hits(101, 130), "nextPageToken": "t2", "isLast": true}),
        );

        let results = search_with_limit(&server, 250).unwrap();

        first.assert_calls(1);
        second.assert_calls(1);
        assert_eq!(search_keys(&results), expected_keys(1, 130));
        assert_eq!(results.is_last, Some(true));
    }

    #[test]
    fn search_stops_when_a_page_has_no_token() {
        for is_last in [Some(false), None] {
            let server = MockServer::start();
            let mut body = serde_json::json!({"issues": search_hits(1, 100)});
            if let Some(is_last) = is_last {
                body["isLast"] = serde_json::json!(is_last);
            }
            let first = search_page_mock(&server, 100, None, 200, body);

            let results = search_with_limit(&server, 250).unwrap();

            first.assert_calls(1);
            assert_eq!(results.issues.len(), 100);
            assert_eq!(results.is_last, is_last);
        }
    }

    #[test]
    fn search_stops_when_a_page_adds_no_issues() {
        let server = MockServer::start();
        let first = search_page_mock(
            &server,
            100,
            None,
            200,
            serde_json::json!({"issues": search_hits(1, 100), "nextPageToken": "t1", "isLast": false}),
        );
        let second = search_page_mock(
            &server,
            100,
            Some("t1"),
            200,
            serde_json::json!({"issues": [], "nextPageToken": "t2", "isLast": false}),
        );

        let results = search_with_limit(&server, 250).unwrap();

        first.assert_calls(1);
        second.assert_calls(1);
        assert_eq!(results.issues.len(), 100);
        assert_eq!(results.is_last, Some(false));
    }

    #[test]
    fn search_follows_a_token_when_jira_omits_is_last() {
        let server = MockServer::start();
        let first = search_page_mock(
            &server,
            100,
            None,
            200,
            serde_json::json!({"issues": search_hits(1, 100), "nextPageToken": "t1"}),
        );
        let second = search_page_mock(
            &server,
            20,
            Some("t1"),
            200,
            serde_json::json!({"issues": search_hits(101, 120), "nextPageToken": "t2"}),
        );

        let results = search_with_limit(&server, 120).unwrap();

        first.assert_calls(1);
        second.assert_calls(1);
        assert_eq!(search_keys(&results), expected_keys(1, 120));
        assert_eq!(results.is_last, None);
    }

    #[test]
    fn missing_or_null_issue_lists_are_empty_results() {
        for body in [
            serde_json::json!({"isLast": true}),
            serde_json::json!({"issues": null, "isLast": true}),
        ] {
            let server = MockServer::start();
            let mock = search_page_mock(&server, 25, None, 200, body);

            let results = search(&server).unwrap();

            mock.assert_calls(1);
            assert!(results.issues.is_empty());
        }
    }

    #[test]
    fn a_failed_second_page_fails_the_search() {
        let cases: [(u16, serde_json::Value, &str); 2] = [
            (
                500,
                serde_json::json!({"errorMessages": ["secret-token leaked"]}),
                "Jira API error (HTTP 500)",
            ),
            (
                400,
                serde_json::json!({"errorMessages": ["The token expired"]}),
                "invalid JQL query (400): The token expired",
            ),
        ];
        for (status, body, expected) in cases {
            let server = MockServer::start();
            let first = search_page_mock(
                &server,
                100,
                None,
                200,
                serde_json::json!({"issues": search_hits(1, 100), "nextPageToken": "t1", "isLast": false}),
            );
            let second = search_page_mock(&server, 100, Some("t1"), status, body);

            let error = search_with_limit(&server, 200).unwrap_err();

            first.assert_calls(1);
            second.assert_calls(1);
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn search_400_surfaces_sanitized_jira_messages_on_one_line() {
        let server = MockServer::start();
        let mock = search_mock(
            &server,
            400,
            serde_json::json!({
                "errorMessages": [
                    "Error in the JQL Query:\r\n\u{1b}[31mThe character '#' is reserved\u{202e}.",
                    " \u{7}\t "
                ],
                "errors": {
                    "summary": "Field\n'summary' is \u{9b}odd",
                    "labels": " \u{7}",
                    "jql": "Expected a value"
                }
            })
            .to_string(),
        );

        let error = search(&server).unwrap_err();

        mock.assert_calls(1);
        assert_eq!(
            error.to_string(),
            "invalid JQL query (400): Error in the JQL Query: [31mThe character '#' is reserved.; jql: Expected a value; summary: Field 'summary' is odd"
        );
    }

    #[test]
    fn search_400_redacts_credentials_but_keeps_the_email() {
        let encoded = AUTH_HEADER.strip_prefix("Basic ").unwrap();
        let server = MockServer::start();
        let mock = search_mock(
            &server,
            400,
            serde_json::json!({
                "errorMessages": [
                    format!("header {AUTH_HEADER} and {encoded}"),
                    "token secret-token, split secret\u{1b}-token",
                    "assignee = jane@example.com is invalid"
                ],
                "errors": {"secret-token": format!("Basic\t{encoded}")}
            })
            .to_string(),
        );

        let error = search(&server).unwrap_err();

        mock.assert_calls(1);
        assert_eq!(
            error.to_string(),
            "invalid JQL query (400): header [REDACTED] and [REDACTED]; token [REDACTED], split [REDACTED]; assignee = jane@example.com is invalid; [REDACTED]: [REDACTED]"
        );
        let debug = format!("{error:?}");
        assert!(!debug.contains("secret-token"), "{debug}");
        assert!(!debug.contains(encoded), "{debug}");
    }

    #[test]
    fn search_400_without_usable_messages_falls_back_to_the_status_error() {
        for body in [
            "not json \u{1b}[31m secret-token".to_owned(),
            serde_json::json!({}).to_string(),
            serde_json::json!({"errorMessages": null, "errors": null}).to_string(),
            serde_json::json!({"errorMessages": [], "errors": {}}).to_string(),
            serde_json::json!({"errorMessages": ["\u{1b}\u{202e}", " "]}).to_string(),
            serde_json::json!({"errors": {"jql": ""}}).to_string(),
            serde_json::json!({"errors": {"jql": " \u{1b}\u{7}\u{202e}\t"}}).to_string(),
            serde_json::json!({"errorMessages": [42]}).to_string(),
        ] {
            let server = MockServer::start();
            let mock = search_mock(&server, 400, body.clone());

            let error = search(&server).unwrap_err();

            mock.assert_calls(1);
            assert!(
                matches!(error, JiraClientError::Api { status: 400 }),
                "{body}: {error:?}"
            );
            assert_eq!(error.to_string(), "Jira API error (HTTP 400)");
        }
    }

    #[test]
    fn search_400_error_bodies_are_bounded() {
        let server = MockServer::start();
        let exact = search_mock(&server, 400, padded_error_body(MAX_ERROR_RESPONSE_BYTES));
        assert_eq!(
            search(&server).unwrap_err().to_string(),
            "invalid JQL query (400): Bad query"
        );
        exact.assert_calls(1);

        let server = MockServer::start();
        let oversized = search_mock(
            &server,
            400,
            padded_error_body(MAX_ERROR_RESPONSE_BYTES + 1),
        );
        assert!(matches!(
            search(&server).unwrap_err(),
            JiraClientError::Api { status: 400 }
        ));
        oversized.assert_calls(1);
    }

    #[test]
    fn search_400_bodies_without_content_length_are_still_bounded() {
        let addr = raw_server(|mut stream| {
            stream
                .write_all(b"HTTP/1.1 400 Bad Request\r\nTransfer-Encoding: chunked\r\n\r\n")
                .unwrap();
            let body = padded_error_body(MAX_ERROR_RESPONSE_BYTES + 1);
            stream
                .write_all(format!("{:x}\r\n{body}\r\n0\r\n\r\n", body.len()).as_bytes())
                .unwrap();
        });
        let client = short_timeout_client(addr, Duration::from_secs(5));

        let error = client.search_issues("project = ABC", 20).unwrap_err();

        assert!(matches!(error, JiraClientError::Api { status: 400 }));
    }

    #[test]
    fn search_maps_other_statuses_without_response_data() {
        let cases: [(u16, &str); 5] = [
            (
                401,
                "authentication failed (401): check JIRA_EMAIL and JIRA_API_TOKEN",
            ),
            (
                403,
                "forbidden (403): the account lacks permission for this operation",
            ),
            (404, "not found (404): issue search"),
            (429, "rate limited by Jira (429): try again later"),
            (500, "Jira API error (HTTP 500)"),
        ];
        for (status, expected) in cases {
            let server = MockServer::start();
            let mock = search_mock(
                &server,
                status,
                serde_json::json!({"errorMessages": ["secret-token leaked"]}).to_string(),
            );

            let error = search(&server).unwrap_err();

            mock.assert_calls(1);
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn other_endpoints_keep_generic_400_errors() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET);
            then.status(400)
                .json_body(serde_json::json!({"errorMessages": ["Field does not exist"]}));
        });

        let error = client(&server).myself().unwrap_err();

        assert!(matches!(error, JiraClientError::Api { status: 400 }));
    }

    #[test]
    fn search_success_responses_are_bounded() {
        let server = MockServer::start();
        let mock = search_mock(&server, 200, " ".repeat(MAX_ATTACHMENT_METADATA_BYTES + 1));

        let error = search(&server).unwrap_err();

        mock.assert_calls(1);
        assert!(matches!(error, JiraClientError::MetadataTooLarge));
    }

    #[test]
    fn invalid_search_json_is_a_metadata_error() {
        let server = MockServer::start();
        let mock = search_mock(&server, 200, r#"{"issues": [{"fields": {}}]}"#);

        let error = search(&server).unwrap_err();

        mock.assert_calls(1);
        assert!(matches!(error, JiraClientError::MetadataJson { .. }));
    }
}
