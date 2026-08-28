use std::fmt;
use std::io::Read;

use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use reqwest::redirect::Policy;
use serde_json::json;
use thiserror::Error;
use url::Url;

use crate::config::{SanitizedUrl, XrayConfig};
use crate::xray::test_runs::{GetTestRunsResponse, TestRunPage};

const USER_AGENT: &str = concat!("jcli/", env!("CARGO_PKG_VERSION"));
const AUTHENTICATE_SEGMENTS: &[&str] = &["api", "v2", "authenticate"];
const GRAPHQL_SEGMENTS: &[&str] = &["api", "v2", "graphql"];
/// Bearer tokens are short JWTs; anything beyond 64 KiB is not one.
const MAX_AUTH_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_GRAPHQL_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Fixed first page. No pagination flags by design for now.
const PAGE_LIMIT: i64 = 100;
const PAGE_START: i64 = 0;

const GET_TEST_RUNS_QUERY: &str = "query GetTestRuns($testIssueIds: [String], $limit: Int!, $start: Int) {\
    \n  getTestRuns(testIssueIds: $testIssueIds, limit: $limit, start: $start) {\
    \n    total\
    \n    start\
    \n    limit\
    \n    results {\
    \n      id\
    \n      status { name }\
    \n      startedOn\
    \n      finishedOn\
    \n      testExecution { issueId }\
    \n    }\
    \n  }\
    \n}";

#[derive(Debug, Error)]
pub enum XrayClientError {
    #[error("failed to build request URL")]
    InvalidUrl,
    #[error("HTTP request failed")]
    Http {
        #[source]
        source: reqwest::Error,
    },
    #[error("Xray authentication failed (HTTP 401): check XRAY_CLIENT_ID and XRAY_CLIENT_SECRET")]
    Unauthorized,
    #[error("forbidden (HTTP 403): the account lacks permission for this Xray operation")]
    Forbidden,
    #[error("not found (HTTP 404): Xray endpoint")]
    NotFound,
    #[error("rate limited by Xray (HTTP 429): try again later")]
    RateLimited,
    #[error("Xray API error (HTTP {status})")]
    Api { status: u16 },
    #[error("Xray response could not be read")]
    ResponseRead {
        #[source]
        source: std::io::Error,
    },
    #[error("Xray response exceeds the safety limit")]
    ResponseTooLarge,
    #[error("Xray authentication response is invalid")]
    InvalidAuthResponse,
    #[error("Xray GraphQL response is invalid")]
    InvalidGraphQlResponse,
    #[error("Xray GraphQL query failed")]
    GraphQl,
}

/// Blocking client for the Xray Cloud API.
///
/// Xray authenticates and queries over POST, so this client cannot be
/// GET-only like the Jira client. It still stays semantically read-only:
/// the only requests are the credential exchange and a GraphQL query, and
/// neither changes Xray data. Redirects are disabled for both because both
/// carry credentials.
///
/// The manual `Debug` implementation prints a sanitized base URL only; the
/// client secret never reaches debug output, and the bearer token never
/// becomes part of the client at all.
pub struct XrayClient {
    http: Client,
    base_url: Url,
    client_id: String,
    client_secret: String,
}

impl fmt::Debug for XrayClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XrayClient")
            .field("base_url", &SanitizedUrl(&self.base_url))
            .finish_non_exhaustive()
    }
}

impl XrayClient {
    pub fn new(config: &XrayConfig) -> Result<Self, XrayClientError> {
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .referer(false)
            .redirect(Policy::none())
            .build()
            .map_err(|source| XrayClientError::Http { source })?;
        Ok(XrayClient {
            http,
            base_url: config.base_url.clone(),
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
        })
    }

    /// Returns the first fixed page of test runs for a test issue's numeric
    /// ID. Resolves its own bearer token; the token stays local to this
    /// call and is dropped afterwards.
    pub fn test_runs(&self, test_issue_id: &str) -> Result<TestRunPage, XrayClientError> {
        let token = self.authenticate()?;
        self.get_test_runs(test_issue_id, &token)
    }

    /// Exchanges the API key credentials for a bearer token. Xray answers
    /// with a bare JSON string that holds the token.
    fn authenticate(&self) -> Result<String, XrayClientError> {
        let url = self.endpoint_url(AUTHENTICATE_SEGMENTS)?;
        let body = json!({
            "client_id": self.client_id,
            "client_secret": self.client_secret,
        });
        let response = self.post_json(url, &body)?;
        let response = check_status(response)?;
        let body = read_bounded_body(response, MAX_AUTH_RESPONSE_BYTES)?;
        serde_json::from_slice(&body).map_err(|_| XrayClientError::InvalidAuthResponse)
    }

    fn get_test_runs(
        &self,
        test_issue_id: &str,
        token: &str,
    ) -> Result<TestRunPage, XrayClientError> {
        let url = self.endpoint_url(GRAPHQL_SEGMENTS)?;
        let body = json!({
            "query": GET_TEST_RUNS_QUERY,
            "variables": {
                "testIssueIds": [test_issue_id],
                "limit": PAGE_LIMIT,
                "start": PAGE_START,
            },
        });
        let response = self.http.post(url).bearer_auth(token).json(&body).send()?;
        let response = check_status(response)?;
        let body = read_bounded_body(response, MAX_GRAPHQL_RESPONSE_BYTES)?;
        let parsed: GetTestRunsResponse =
            serde_json::from_slice(&body).map_err(|_| XrayClientError::InvalidGraphQlResponse)?;
        // GraphQL reports failures in the body while keeping HTTP 200.
        // The server message is deliberately discarded: it may echo
        // credentials or untrusted input, so only a generic error is
        // reported.
        if !parsed.errors.is_empty() {
            return Err(XrayClientError::GraphQl);
        }
        parsed
            .data
            .and_then(|data| data.get_test_runs)
            .ok_or(XrayClientError::InvalidGraphQlResponse)
    }

    fn post_json(&self, url: Url, body: &serde_json::Value) -> Result<Response, XrayClientError> {
        self.http
            .post(url)
            .json(body)
            .send()
            .map_err(|source| XrayClientError::Http { source })
    }

    fn endpoint_url(&self, segments: &[&str]) -> Result<Url, XrayClientError> {
        let mut url = self.base_url.clone();
        url.set_query(None);
        url.set_fragment(None);
        let mut path = url
            .path_segments_mut()
            .map_err(|_| XrayClientError::InvalidUrl)?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
        drop(path);
        Ok(url)
    }
}

impl From<reqwest::Error> for XrayClientError {
    fn from(source: reqwest::Error) -> Self {
        XrayClientError::Http { source }
    }
}

fn check_status(response: Response) -> Result<Response, XrayClientError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    Err(match status {
        StatusCode::UNAUTHORIZED => XrayClientError::Unauthorized,
        StatusCode::FORBIDDEN => XrayClientError::Forbidden,
        StatusCode::NOT_FOUND => XrayClientError::NotFound,
        StatusCode::TOO_MANY_REQUESTS => XrayClientError::RateLimited,
        other => XrayClientError::Api {
            status: other.as_u16(),
        },
    })
}

/// Reads at most `limit` bytes of the body. The response is read fully into
/// memory (bounded), so error paths never need to touch the body and no
/// response data can leak into error messages.
fn read_bounded_body(response: Response, limit: usize) -> Result<Vec<u8>, XrayClientError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(XrayClientError::ResponseTooLarge);
    }

    let mut body = Vec::new();
    response
        .take(limit as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|source| XrayClientError::ResponseRead { source })?;
    if body.len() > limit {
        return Err(XrayClientError::ResponseTooLarge);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::POST;
    use httpmock::MockServer;

    const CLIENT_ID: &str = "xray-client-id";
    const CLIENT_SECRET: &str = "xray-client-secret";
    const TOKEN: &str = "xray-bearer-token";

    fn client(server: &MockServer) -> XrayClient {
        XrayClient::new(&XrayConfig {
            base_url: Url::parse(&server.base_url()).unwrap(),
            client_id: CLIENT_ID.to_owned(),
            client_secret: CLIENT_SECRET.to_owned(),
        })
        .unwrap()
    }

    fn auth_mock(server: &MockServer) -> httpmock::Mock<'_> {
        server.mock(|when, then| {
            when.method(POST)
                .path("/api/v2/authenticate")
                .header("User-Agent", USER_AGENT)
                .json_body(serde_json::json!({
                    "client_id": CLIENT_ID,
                    "client_secret": CLIENT_SECRET,
                }));
            then.status(200).body(format!("\"{TOKEN}\""));
        })
    }

    fn graphql_mock(server: &MockServer, then_body: serde_json::Value) -> httpmock::Mock<'_> {
        server.mock(|when, then| {
            when.method(POST)
                .path("/api/v2/graphql")
                .header("Authorization", format!("Bearer {TOKEN}"))
                .json_body(serde_json::json!({
                    "query": GET_TEST_RUNS_QUERY,
                    "variables": {"testIssueIds": ["10400"], "limit": 100, "start": 0},
                }));
            then.status(200).json_body(then_body);
        })
    }

    fn page_body(total: i64, result_count: usize) -> serde_json::Value {
        serde_json::json!({
            "data": {"getTestRuns": {
                "total": total,
                "start": 0,
                "limit": 100,
                "results": (0..result_count).map(|index| serde_json::json!({
                    "id": index.to_string(),
                    "status": {"name": "PASSED"},
                    "startedOn": "2024-05-01T08:00:00+02:00",
                    "finishedOn": null,
                    "testExecution": {"issueId": "10500"}
                })).collect::<Vec<_>>()
            }}
        })
    }

    #[test]
    fn test_runs_authenticates_then_queries_with_the_bearer_token() {
        let server = MockServer::start();
        let auth = auth_mock(&server);
        let graphql = graphql_mock(&server, page_body(1, 1));

        let page = match client(&server).test_runs("10400") {
            Ok(page) => page,
            Err(error) => {
                eprintln!("error: {error:?}");
                panic!("failed");
            }
        };

        auth.assert_calls(1);
        graphql.assert_calls(1);
        assert_eq!(page.total, Some(1));
        assert_eq!(page.start, Some(0));
        assert_eq!(page.limit, Some(100));
        let run = page.results.as_ref().unwrap()[0].as_ref().unwrap();
        assert_eq!(run.id.as_deref(), Some("0"));
        assert_eq!(run.status.as_ref().unwrap().name.as_deref(), Some("PASSED"));
        assert_eq!(run.started_on.as_deref(), Some("2024-05-01T08:00:00+02:00"));
        assert_eq!(run.finished_on, None);
        assert_eq!(
            run.test_execution.as_ref().unwrap().issue_id.as_deref(),
            Some("10500")
        );
    }

    #[test]
    fn pagination_is_fixed_and_makes_exactly_one_request() {
        let server = MockServer::start();
        let auth = auth_mock(&server);
        let graphql = graphql_mock(&server, page_body(0, 0));

        let page = match client(&server).test_runs("10400") {
            Ok(page) => page,
            Err(error) => {
                eprintln!("error: {error:?}");
                panic!("failed");
            }
        };

        auth.assert_calls(1);
        graphql.assert_calls(1);
        assert!(page.results.as_ref().unwrap().is_empty());
    }

    #[test]
    fn authentication_posts_json_credentials_and_parses_the_string_token() {
        // Covered end to end by `test_runs_authenticates_then_queries…`;
        // this test pins the auth request shape on its own so a broken
        // auth mock fails here with a clear message.
        let server = MockServer::start();
        let auth = auth_mock(&server);
        let graphql = graphql_mock(&server, page_body(0, 0));

        client(&server).test_runs("10400").unwrap();

        auth.assert();
        graphql.assert();
    }

    #[test]
    fn an_auth_response_that_is_not_a_json_string_is_invalid() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/api/v2/authenticate");
            then.status(200).json_body(serde_json::json!({
                "token": "wrapped-not-bare"
            }));
        });

        let error = client(&server).test_runs("10400").unwrap_err();

        assert!(matches!(error, XrayClientError::InvalidAuthResponse));
    }

    #[test]
    fn an_oversized_auth_response_is_rejected() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/api/v2/authenticate");
            then.status(200)
                .body(vec![b'a'; MAX_AUTH_RESPONSE_BYTES + 1]);
        });

        let error = client(&server).test_runs("10400").unwrap_err();

        assert!(matches!(error, XrayClientError::ResponseTooLarge));
    }

    #[test]
    fn an_oversized_graphql_response_is_rejected() {
        let server = MockServer::start();
        let _auth = auth_mock(&server);
        server.mock(|when, then| {
            when.method(POST).path("/api/v2/graphql");
            then.status(200)
                .body(vec![b' '; MAX_GRAPHQL_RESPONSE_BYTES + 1]);
        });

        let error = client(&server).test_runs("10400").unwrap_err();

        assert!(matches!(error, XrayClientError::ResponseTooLarge));
    }

    #[test]
    fn a_graphql_response_without_data_is_invalid() {
        let server = MockServer::start();
        let _auth = auth_mock(&server);
        let _graphql = graphql_mock(&server, serde_json::json!({"data": null}));

        let error = client(&server).test_runs("10400").unwrap_err();

        assert!(matches!(error, XrayClientError::InvalidGraphQlResponse));
    }

    #[test]
    fn graphql_errors_are_detected_on_http_200_and_reported_generically() {
        let server = MockServer::start();
        let _auth = auth_mock(&server);
        let _graphql = graphql_mock(
            &server,
            serde_json::json!({"errors": [{"message": "Unknown field 'getTestRuns'"}]}),
        );

        let error = client(&server).test_runs("10400").unwrap_err();

        assert_eq!(error.to_string(), "Xray GraphQL query failed");
    }

    #[test]
    fn graphql_error_messages_never_reach_the_error_output() {
        // The remote message pretends to leak everything sensitive the
        // client holds, plus control characters, bidi formatting code
        // points, and filler text. None of it may reach the rendered error.
        let server = MockServer::start();
        let _auth = auth_mock(&server);
        let remote_message = format!(
            "{CLIENT_SECRET} {TOKEN} \u{1b}[31m line-one\r\nline-two\t\
             \u{061C}\u{200E}\u{200F}\u{202A}\u{202B}\u{202C}\
             \u{202D}\u{202E}\u{2066}\u{2067}\u{2068}\u{2069}\
             and a substantial amount of untrusted text follows here"
        );
        let _graphql = graphql_mock(
            &server,
            serde_json::json!({"errors": [{"message": remote_message}]}),
        );

        let error = client(&server).test_runs("10400").unwrap_err();
        let rendered = error.to_string();

        assert_eq!(rendered, "Xray GraphQL query failed");
        assert!(!rendered.contains(CLIENT_SECRET));
        assert!(!rendered.contains(TOKEN));
        assert!(!rendered.contains("line-one"));
        assert!(!rendered.contains("untrusted"));
        assert!(!rendered.contains('\u{1b}'));
        for bidi in [
            '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}',
            '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        ] {
            assert!(!rendered.contains(bidi));
        }
    }

    #[test]
    fn redirects_are_not_followed_and_reach_no_other_endpoint() {
        let server = MockServer::start();
        let auth_redirect = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v2/authenticate")
                .json_body(serde_json::json!({
                    "client_id": CLIENT_ID,
                    "client_secret": CLIENT_SECRET,
                }));
            then.status(302).header("Location", "/stolen-endpoint");
        });
        let stolen_endpoint = server.mock(|when, then| {
            when.method(POST).path("/stolen-endpoint");
            then.status(200).body("\"token\"");
        });
        let graphql = graphql_mock(&server, page_body(0, 0));

        let error = client(&server).test_runs("10400").unwrap_err();

        auth_redirect.assert_calls(1);
        stolen_endpoint.assert_calls(0);
        graphql.assert_calls(0);
        assert!(matches!(error, XrayClientError::Api { status: 302 }));
    }

    #[test]
    fn graphql_status_errors_map_to_typed_variants_without_response_data() {
        for (status, expected) in [
            (
                401u16,
                "Xray authentication failed (HTTP 401): check XRAY_CLIENT_ID and XRAY_CLIENT_SECRET",
            ),
            (
                403,
                "forbidden (HTTP 403): the account lacks permission for this Xray operation",
            ),
            (404, "not found (HTTP 404): Xray endpoint"),
            (429, "rate limited by Xray (HTTP 429): try again later"),
            (500, "Xray API error (HTTP 500)"),
        ] {
            let server = MockServer::start();
            let _auth = auth_mock(&server);
            let leaky_body = format!("{CLIENT_SECRET} {TOKEN} untrusted\r\n\u{1b}[31mdata");
            server.mock(|when, then| {
                when.method(POST).path("/api/v2/graphql");
                then.status(status).body(leaky_body.clone());
            });

            let error = client(&server).test_runs("10400").unwrap_err();
            let rendered = error.to_string();

            assert_eq!(rendered, expected);
            assert!(!rendered.contains(CLIENT_SECRET));
            assert!(!rendered.contains(TOKEN));
            assert!(!rendered.contains("untrusted"));
            assert!(!rendered.contains('\u{1b}'));
        }
    }

    #[test]
    fn auth_status_errors_map_to_typed_variants() {
        for status in [401u16, 403, 404, 429, 500] {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(POST).path("/api/v2/authenticate");
                then.status(status).body("untrusted response data");
            });

            let error = client(&server).test_runs("10400").unwrap_err();
            let expected = match status {
                401 => {
                    "Xray authentication failed (HTTP 401): check XRAY_CLIENT_ID and XRAY_CLIENT_SECRET"
                }
                403 => "forbidden (HTTP 403): the account lacks permission for this Xray operation",
                404 => "not found (HTTP 404): Xray endpoint",
                429 => "rate limited by Xray (HTTP 429): try again later",
                _ => "Xray API error (HTTP 500)",
            };
            assert_eq!(error.to_string(), expected);
            assert!(!error.to_string().contains("untrusted"));
        }
    }

    #[test]
    fn debug_output_never_contains_secrets_or_raw_url_parts() {
        let config = XrayConfig {
            base_url: Url::parse("https://user:pass@example.com/xray/?unsafe=1#fragment").unwrap(),
            client_id: CLIENT_ID.to_owned(),
            client_secret: CLIENT_SECRET.to_owned(),
        };
        let rendered = format!("{:?}", XrayClient::new(&config).unwrap());
        assert!(rendered.contains("https://example.com/xray/"));
        assert!(!rendered.contains(CLIENT_ID));
        assert!(!rendered.contains(CLIENT_SECRET));
        assert!(!rendered.contains("user:pass"));
        assert!(!rendered.contains("unsafe=1"));
        assert!(!rendered.contains("fragment"));
    }

    #[test]
    fn endpoint_urls_append_to_a_base_url_with_a_path_prefix() {
        let config = XrayConfig {
            base_url: Url::parse("https://example.com/xray/").unwrap(),
            client_id: CLIENT_ID.to_owned(),
            client_secret: CLIENT_SECRET.to_owned(),
        };
        let client = XrayClient::new(&config).unwrap();

        let url = client.endpoint_url(AUTHENTICATE_SEGMENTS).unwrap();

        assert_eq!(url.as_str(), "https://example.com/xray/api/v2/authenticate");
    }

    #[test]
    fn endpoint_urls_discard_base_query_and_fragment() {
        let config = XrayConfig {
            base_url: Url::parse("https://example.com/?unsafe=1#fragment").unwrap(),
            client_id: CLIENT_ID.to_owned(),
            client_secret: CLIENT_SECRET.to_owned(),
        };
        let client = XrayClient::new(&config).unwrap();

        let url = client.endpoint_url(GRAPHQL_SEGMENTS).unwrap();

        assert_eq!(url.as_str(), "https://example.com/api/v2/graphql");
    }
}
