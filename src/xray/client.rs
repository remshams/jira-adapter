use std::fmt;
use std::io::Read;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use reqwest::redirect::Policy;
use serde_json::json;
use thiserror::Error;
use url::Url;

use crate::config::{SanitizedUrl, XrayConfig};
use crate::xray::test_runs::{GetTestRunsResponse, TestRunPage, XrayIssueKind};

const USER_AGENT: &str = concat!("jcli/", env!("CARGO_PKG_VERSION"));
const AUTHENTICATE_SEGMENTS: &[&str] = &["api", "v2", "authenticate"];
const GRAPHQL_SEGMENTS: &[&str] = &["api", "v2", "graphql"];
/// Stops a connection attempt to an unreachable Xray endpoint early.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Caps the whole request (connect, send, and response read) so a stalled
/// Xray server cannot hang the CLI indefinitely.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Bearer tokens are short JWTs; anything beyond 64 KiB is not one.
const MAX_AUTH_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_GRAPHQL_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// The most runs jcli asks Xray for in one `getTestRuns` request.
const PAGE_SIZE: usize = 100;

/// Fields every query selects per run.
const RUN_FIELDS: &str = "      id\
    \n      status { name }\
    \n      startedOn\
    \n      finishedOn\
    \n      testExecution { issueId }\
    \n";

/// Fields `--details` adds per run: the comment, defect issue IDs,
/// evidence, manual step results and automated results with their log.
const RUN_DETAIL_FIELDS: &str = "      comment\
    \n      defects\
    \n      evidence { filename downloadLink }\
    \n      steps {\
    \n        status { name }\
    \n        actualResult\
    \n        comment\
    \n        evidence { filename downloadLink }\
    \n      }\
    \n      results {\
    \n        name\
    \n        status { name }\
    \n        log\
    \n        duration\
    \n      }\
    \n";

/// The name of the issue ID filter for a kind. It names both the
/// `getTestRuns` argument and the GraphQL variable that carries the ID.
fn issue_ids_filter(kind: XrayIssueKind) -> &'static str {
    match kind {
        XrayIssueKind::Test => "testIssueIds",
        XrayIssueKind::TestExecution => "testExecIssueIds",
    }
}

/// The `getTestRuns` query text for a filter. A Test's runs span all Test
/// Executions; a Test Execution's runs are the ones it contains. Both
/// share the page shape and selection set. Tests pin all four texts.
fn get_test_runs_query(filter: &str, details: bool) -> String {
    let detail_fields = if details { RUN_DETAIL_FIELDS } else { "" };
    format!(
        "query GetTestRuns(${filter}: [String], $statuses: [String], $limit: Int!, $start: Int) {{\
        \n  getTestRuns({filter}: ${filter}, statuses: $statuses, limit: $limit, start: $start) {{\
        \n    total\
        \n    start\
        \n    limit\
        \n    results {{\
        \n{RUN_FIELDS}{detail_fields}    }}\
        \n  }}\
        \n}}"
    )
}

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
        Self::build(config, CONNECT_TIMEOUT, REQUEST_TIMEOUT)
    }

    #[cfg(test)]
    fn with_timeouts(
        config: &XrayConfig,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Self, XrayClientError> {
        Self::build(config, connect_timeout, request_timeout)
    }

    fn build(
        config: &XrayConfig,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Self, XrayClientError> {
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .referer(false)
            .redirect(Policy::none())
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .build()
            .map_err(http_error)?;
        Ok(XrayClient {
            http,
            base_url: config.base_url.clone(),
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
        })
    }

    /// Returns up to `limit` test runs for a Test or Test Execution issue's
    /// numeric ID; `kind` picks the matching filter. Resolves its own bearer
    /// token; the token stays local to this call and is dropped afterwards.
    /// `statuses` filters server-side when given and is sent as a GraphQL
    /// variable, never as query text. `details` selects the run's comment,
    /// defects, evidence, steps and results.
    ///
    /// Requests pages of at most 100 runs. Every page sends the same query
    /// and variables except `start` and `limit`. The runs of all pages are
    /// merged into one page: `total` is the last page's total, `start` is 0
    /// and `limit` is the requested limit. A failed page fails the call.
    pub fn test_runs(
        &self,
        kind: XrayIssueKind,
        issue_id: &str,
        statuses: Option<&[String]>,
        details: bool,
        limit: usize,
    ) -> Result<TestRunPage, XrayClientError> {
        let token = self.authenticate()?;
        let filter = issue_ids_filter(kind);
        // Absent statuses become JSON null so the variable is always set;
        // values never reach the query text itself.
        let body = json!({
            "query": get_test_runs_query(filter, details),
            "variables": {
                filter: [issue_id],
                "statuses": statuses,
            },
        });
        self.collect_pages(body, &token, limit)
    }

    /// Requests pages of `body`'s query with increasing `start` until a stop
    /// rule holds, and merges their runs into one page.
    fn collect_pages(
        &self,
        mut body: serde_json::Value,
        token: &str,
        limit: usize,
    ) -> Result<TestRunPage, XrayClientError> {
        let mut runs = Vec::new();
        let mut total = None;
        // Every page before the final one adds at least one run, so `limit`
        // pages are always enough. The bound keeps the loop finite even when
        // the stop rule is broken, so such a mutant fails a test instead of
        // hanging it on a mock that keeps answering the same request.
        for _ in 0..limit {
            let start = runs.len();
            body["variables"]["start"] = json!(start);
            body["variables"]["limit"] = json!((limit - start).min(PAGE_SIZE));
            let page = self.get_test_runs(&body, token)?;
            let page_runs = page.results.unwrap_or_default();
            let last = is_final_page(page.total, start, page_runs.len());
            total = page.total;
            runs.extend(page_runs);
            if last || runs.len() >= limit {
                break;
            }
        }
        Ok(TestRunPage {
            total,
            start: Some(0),
            limit: Some(limit as i64),
            results: Some(runs),
        })
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

    /// Sends one `getTestRuns` request and parses its page.
    fn get_test_runs(
        &self,
        body: &serde_json::Value,
        token: &str,
    ) -> Result<TestRunPage, XrayClientError> {
        let url = self.endpoint_url(GRAPHQL_SEGMENTS)?;
        let response = self.http.post(url).bearer_auth(token).json(body).send()?;
        let response = check_status(response)?;
        let body = read_bounded_body(response, MAX_GRAPHQL_RESPONSE_BYTES)?;
        parse_test_runs(&body)
    }

    fn post_json(&self, url: Url, body: &serde_json::Value) -> Result<Response, XrayClientError> {
        Ok(self.http.post(url).json(body).send()?)
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

/// Wraps a reqwest error as an [`XrayClientError::Http`] with the request
/// URL removed. reqwest embeds the full URL (including userinfo and query)
/// in its Display and Debug output, so it must be stripped before the error
/// can be stored or logged.
fn http_error(source: reqwest::Error) -> XrayClientError {
    XrayClientError::Http {
        source: source.without_url(),
    }
}

impl From<reqwest::Error> for XrayClientError {
    fn from(source: reqwest::Error) -> Self {
        http_error(source)
    }
}

/// Paging stops after a page without runs, after a page without a total,
/// and after the page that reaches Xray's total. The empty-page rule keeps
/// a server that reports too high a total from keeping jcli in the loop.
fn is_final_page(total: Option<i64>, start: usize, returned: usize) -> bool {
    returned == 0 || total.is_none_or(|total| (start + returned) as i64 >= total)
}

fn parse_test_runs(body: &[u8]) -> Result<TestRunPage, XrayClientError> {
    let parsed: GetTestRunsResponse =
        serde_json::from_slice(body).map_err(|_| XrayClientError::InvalidGraphQlResponse)?;
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

fn check_status(response: Response) -> Result<Response, XrayClientError> {
    let status = response.status();
    if status.is_success() {
        Ok(response)
    } else {
        Err(status_error(status))
    }
}

fn status_error(status: StatusCode) -> XrayClientError {
    match status {
        StatusCode::UNAUTHORIZED => XrayClientError::Unauthorized,
        StatusCode::FORBIDDEN => XrayClientError::Forbidden,
        StatusCode::NOT_FOUND => XrayClientError::NotFound,
        StatusCode::TOO_MANY_REQUESTS => XrayClientError::RateLimited,
        other => XrayClientError::Api {
            status: other.as_u16(),
        },
    }
}

/// Reads at most `limit` bytes of the body. The response is read fully into
/// memory (bounded), so error paths never need to touch the body and no
/// response data can leak into error messages.
fn read_bounded_body(response: Response, limit: usize) -> Result<Vec<u8>, XrayClientError> {
    if content_length_exceeds(response.content_length(), limit) {
        return Err(XrayClientError::ResponseTooLarge);
    }
    read_bounded(response.take(limit as u64 + 1), limit)
}

fn content_length_exceeds(content_length: Option<u64>, limit: usize) -> bool {
    content_length.is_some_and(|length| length > limit as u64)
}

/// Reads at most `limit` bytes from `source`, rejecting anything longer.
/// Works without a Content-Length header too: chunked or streamed bodies
/// are counted as they arrive, so `take` stops after limit + 1 bytes and
/// the overrun is detected below.
fn read_bounded(mut source: impl Read, limit: usize) -> Result<Vec<u8>, XrayClientError> {
    let mut body = Vec::new();
    source
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
    use std::error::Error as _;
    use std::io::{Cursor, Write};
    use std::net::TcpListener;
    use std::thread;

    /// The variable declaration and argument list pinned verbatim against
    /// the official Xray Cloud getTestRuns schema: `statuses` is a nullable
    /// `[String]` argument, so it may be sent as JSON null. These literals
    /// are independent of the query builder and of every mock helper, so
    /// changing the builder cannot make the mocks pass tautologically.
    const DOCUMENTED_GET_TEST_RUNS_FOR_TEST_QUERY: &str = r#"query GetTestRuns($testIssueIds: [String], $statuses: [String], $limit: Int!, $start: Int) {
  getTestRuns(testIssueIds: $testIssueIds, statuses: $statuses, limit: $limit, start: $start) {
    total
    start
    limit
    results {
      id
      status { name }
      startedOn
      finishedOn
      testExecution { issueId }
    }
  }
}"#;

    /// `testExecIssueIds` is the schema's nullable `[String]` filter for
    /// Test Execution issue IDs.
    const DOCUMENTED_GET_TEST_RUNS_FOR_TEST_EXECUTION_QUERY: &str = r#"query GetTestRuns($testExecIssueIds: [String], $statuses: [String], $limit: Int!, $start: Int) {
  getTestRuns(testExecIssueIds: $testExecIssueIds, statuses: $statuses, limit: $limit, start: $start) {
    total
    start
    limit
    results {
      id
      status { name }
      startedOn
      finishedOn
      testExecution { issueId }
    }
  }
}"#;

    /// The `--details` query for a Test. The added fields follow the Xray
    /// Cloud GraphQL docs for `TestRun` (`comment: String`,
    /// `defects: [String]`, `evidence: [Evidence]`, `steps: [TestRunStep]`,
    /// `results: [Result]`), `Evidence` (`filename`, `downloadLink`),
    /// `TestRunStep` (`status: StepStatus`, `actualResult`, `comment`,
    /// `evidence`) and `Result` (`name`, `status: StepStatus`, `log`,
    /// `duration: Float`). They are not yet checked against a real response.
    const DOCUMENTED_GET_TEST_RUNS_FOR_TEST_DETAILS_QUERY: &str = r#"query GetTestRuns($testIssueIds: [String], $statuses: [String], $limit: Int!, $start: Int) {
  getTestRuns(testIssueIds: $testIssueIds, statuses: $statuses, limit: $limit, start: $start) {
    total
    start
    limit
    results {
      id
      status { name }
      startedOn
      finishedOn
      testExecution { issueId }
      comment
      defects
      evidence { filename downloadLink }
      steps {
        status { name }
        actualResult
        comment
        evidence { filename downloadLink }
      }
      results {
        name
        status { name }
        log
        duration
      }
    }
  }
}"#;

    /// The `--details` query for a Test Execution: the same selection set
    /// with the `testExecIssueIds` filter.
    const DOCUMENTED_GET_TEST_RUNS_FOR_TEST_EXECUTION_DETAILS_QUERY: &str = r#"query GetTestRuns($testExecIssueIds: [String], $statuses: [String], $limit: Int!, $start: Int) {
  getTestRuns(testExecIssueIds: $testExecIssueIds, statuses: $statuses, limit: $limit, start: $start) {
    total
    start
    limit
    results {
      id
      status { name }
      startedOn
      finishedOn
      testExecution { issueId }
      comment
      defects
      evidence { filename downloadLink }
      steps {
        status { name }
        actualResult
        comment
        evidence { filename downloadLink }
      }
      results {
        name
        status { name }
        log
        duration
      }
    }
  }
}"#;

    fn query_for(kind: XrayIssueKind, details: bool) -> String {
        get_test_runs_query(issue_ids_filter(kind), details)
    }

    #[test]
    fn the_get_test_runs_queries_match_the_documented_schema_shape() {
        assert_eq!(
            query_for(XrayIssueKind::Test, false),
            DOCUMENTED_GET_TEST_RUNS_FOR_TEST_QUERY
        );
        assert_eq!(
            query_for(XrayIssueKind::TestExecution, false),
            DOCUMENTED_GET_TEST_RUNS_FOR_TEST_EXECUTION_QUERY
        );
        assert_eq!(
            query_for(XrayIssueKind::Test, true),
            DOCUMENTED_GET_TEST_RUNS_FOR_TEST_DETAILS_QUERY
        );
        assert_eq!(
            query_for(XrayIssueKind::TestExecution, true),
            DOCUMENTED_GET_TEST_RUNS_FOR_TEST_EXECUTION_DETAILS_QUERY
        );
    }

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
        graphql_mock_with_statuses(server, then_body, None)
    }

    /// Mocks a GraphQL request whose body must match exactly, so the test
    /// fails if the query text or any variable (including `statuses`) drifts.
    fn graphql_mock_with_statuses<'a>(
        server: &'a MockServer,
        then_body: serde_json::Value,
        statuses: Option<&'a [&'a str]>,
    ) -> httpmock::Mock<'a> {
        let expected_body = serde_json::json!({
            "query": DOCUMENTED_GET_TEST_RUNS_FOR_TEST_QUERY,
            "variables": {
                "testIssueIds": ["10400"],
                "statuses": statuses,
                "limit": 100,
                "start": 0,
            },
        });
        server.mock(|when, then| {
            when.method(POST)
                .path("/api/v2/graphql")
                .header("Authorization", format!("Bearer {TOKEN}"))
                .json_body(expected_body);
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
    fn absent_statuses_are_sent_as_a_null_variable() {
        let server = MockServer::start();
        let _auth = auth_mock(&server);
        let graphql = graphql_mock_with_statuses(&server, page_body(0, 0), None);

        client(&server)
            .test_runs(XrayIssueKind::Test, "10400", None, false, 100)
            .unwrap();

        graphql.assert_calls(1);
    }

    #[test]
    fn one_status_is_sent_as_a_one_element_array_variable() {
        let server = MockServer::start();
        let _auth = auth_mock(&server);
        let graphql = graphql_mock_with_statuses(&server, page_body(0, 0), Some(&["PASSED"]));

        client(&server)
            .test_runs(
                XrayIssueKind::Test,
                "10400",
                Some(&["PASSED".to_owned()]),
                false,
                100,
            )
            .unwrap();

        graphql.assert_calls(1);
    }

    #[test]
    fn multiple_statuses_keep_order_and_send_custom_ids_unchanged() {
        let server = MockServer::start();
        let _auth = auth_mock(&server);
        let graphql = graphql_mock_with_statuses(
            &server,
            page_body(0, 0),
            Some(&["FAILED", "CUSTOM-42", "IN_PROGRESS"]),
        );

        client(&server)
            .test_runs(
                XrayIssueKind::Test,
                "10400",
                Some(&[
                    "FAILED".to_owned(),
                    "CUSTOM-42".to_owned(),
                    "IN_PROGRESS".to_owned(),
                ]),
                false,
                100,
            )
            .unwrap();

        graphql.assert_calls(1);
    }

    #[test]
    fn a_test_queries_by_test_issue_ids_only() {
        let server = MockServer::start();
        let _auth = auth_mock(&server);
        let graphql = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v2/graphql")
                .json_body(serde_json::json!({
                    "query": DOCUMENTED_GET_TEST_RUNS_FOR_TEST_QUERY,
                    "variables": {
                        "testIssueIds": ["10400"],
                        "statuses": null,
                        "limit": 100,
                        "start": 0,
                    },
                }));
            then.status(200).json_body(page_body(0, 0));
        });

        client(&server)
            .test_runs(XrayIssueKind::Test, "10400", None, false, 100)
            .unwrap();

        graphql.assert_calls(1);
    }

    #[test]
    fn a_test_execution_queries_by_test_execution_issue_ids_only() {
        let server = MockServer::start();
        let _auth = auth_mock(&server);
        let graphql = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v2/graphql")
                .header("Authorization", format!("Bearer {TOKEN}"))
                .json_body(serde_json::json!({
                    "query": DOCUMENTED_GET_TEST_RUNS_FOR_TEST_EXECUTION_QUERY,
                    "variables": {
                        "testExecIssueIds": ["10500"],
                        "statuses": ["FAILED"],
                        "limit": 100,
                        "start": 0,
                    },
                }));
            then.status(200).json_body(page_body(1, 1));
        });

        let page = client(&server)
            .test_runs(
                XrayIssueKind::TestExecution,
                "10500",
                Some(&["FAILED".to_owned()]),
                false,
                100,
            )
            .unwrap();

        graphql.assert_calls(1);
        assert_eq!(page.total, Some(1));
    }

    /// Mocks a GraphQL request whose body must equal `query` with the given
    /// filter and the default variables.
    fn exact_query_mock<'a>(
        server: &'a MockServer,
        query: &str,
        filter: &str,
        then_body: serde_json::Value,
    ) -> httpmock::Mock<'a> {
        let expected_body = serde_json::json!({
            "query": query,
            "variables": {
                filter: ["10400"],
                "statuses": null,
                "limit": 100,
                "start": 0,
            },
        });
        server.mock(|when, then| {
            when.method(POST)
                .path("/api/v2/graphql")
                .header("Authorization", format!("Bearer {TOKEN}"))
                .json_body(expected_body);
            then.status(200).json_body(then_body);
        })
    }

    #[test]
    fn details_select_the_detail_fields_for_each_kind() {
        for (kind, query, filter) in [
            (
                XrayIssueKind::Test,
                DOCUMENTED_GET_TEST_RUNS_FOR_TEST_DETAILS_QUERY,
                "testIssueIds",
            ),
            (
                XrayIssueKind::TestExecution,
                DOCUMENTED_GET_TEST_RUNS_FOR_TEST_EXECUTION_DETAILS_QUERY,
                "testExecIssueIds",
            ),
        ] {
            let server = MockServer::start();
            let _auth = auth_mock(&server);
            let graphql = exact_query_mock(&server, query, filter, page_body(0, 0));

            client(&server)
                .test_runs(kind, "10400", None, true, 100)
                .unwrap();

            graphql.assert_calls(1);
        }
    }

    #[test]
    fn without_details_the_query_selects_no_detail_fields() {
        for (kind, query, filter) in [
            (
                XrayIssueKind::Test,
                DOCUMENTED_GET_TEST_RUNS_FOR_TEST_QUERY,
                "testIssueIds",
            ),
            (
                XrayIssueKind::TestExecution,
                DOCUMENTED_GET_TEST_RUNS_FOR_TEST_EXECUTION_QUERY,
                "testExecIssueIds",
            ),
        ] {
            let server = MockServer::start();
            let _auth = auth_mock(&server);
            let graphql = exact_query_mock(&server, query, filter, page_body(0, 0));

            client(&server)
                .test_runs(kind, "10400", None, false, 100)
                .unwrap();

            graphql.assert_calls(1);
        }
    }

    #[test]
    fn detail_fields_of_a_run_are_parsed() {
        let server = MockServer::start();
        let _auth = auth_mock(&server);
        let _graphql = exact_query_mock(
            &server,
            DOCUMENTED_GET_TEST_RUNS_FOR_TEST_DETAILS_QUERY,
            "testIssueIds",
            serde_json::json!({"data": {"getTestRuns": {
                "total": 1, "start": 0, "limit": 100,
                "results": [{
                    "id": "1",
                    "comment": "flaky",
                    "defects": ["10600", null],
                    "evidence": [{"filename": "run.log", "downloadLink": "https://xray/run.log"}],
                    "steps": [{
                        "status": {"name": "FAILED"},
                        "actualResult": "500",
                        "comment": "step note",
                        "evidence": [null]
                    }],
                    "results": [{
                        "name": "login works",
                        "status": {"name": "FAILED"},
                        "log": "AssertionError: expected 200",
                        "duration": 1.5
                    }]
                }]
            }}}),
        );

        let page = client(&server)
            .test_runs(XrayIssueKind::Test, "10400", None, true, 100)
            .unwrap();

        let run = page.results.as_ref().unwrap()[0].as_ref().unwrap();
        assert_eq!(run.comment.as_deref(), Some("flaky"));
        assert_eq!(run.defects, Some(vec![Some("10600".to_owned()), None]));
        let evidence = run.evidence.as_ref().unwrap()[0].as_ref().unwrap();
        assert_eq!(evidence.filename.as_deref(), Some("run.log"));
        assert_eq!(
            evidence.download_link.as_deref(),
            Some("https://xray/run.log")
        );
        let step = run.steps.as_ref().unwrap()[0].as_ref().unwrap();
        assert_eq!(
            step.status.as_ref().unwrap().name.as_deref(),
            Some("FAILED")
        );
        assert_eq!(step.actual_result.as_deref(), Some("500"));
        assert_eq!(step.comment.as_deref(), Some("step note"));
        assert!(step.evidence.as_ref().unwrap()[0].is_none());
        let result = run.results.as_ref().unwrap()[0].as_ref().unwrap();
        assert_eq!(result.name.as_deref(), Some("login works"));
        assert_eq!(
            result.status.as_ref().unwrap().name.as_deref(),
            Some("FAILED")
        );
        assert_eq!(result.log.as_deref(), Some("AssertionError: expected 200"));
        assert_eq!(result.duration, Some(1.5));
    }

    #[test]
    fn response_limits_match_the_api_contract() {
        assert_eq!(MAX_AUTH_RESPONSE_BYTES, 65_536);
        assert_eq!(MAX_GRAPHQL_RESPONSE_BYTES, 16_777_216);
    }

    #[test]
    fn content_length_limit_accepts_the_boundary() {
        assert!(!content_length_exceeds(None, 20));
        assert!(!content_length_exceeds(Some(20), 20));
        assert!(content_length_exceeds(Some(21), 20));
    }

    #[test]
    fn test_runs_authenticates_then_queries_with_the_bearer_token() {
        let server = MockServer::start();
        let auth = auth_mock(&server);
        let graphql = graphql_mock(&server, page_body(1, 1));

        let page = match client(&server).test_runs(XrayIssueKind::Test, "10400", None, false, 100) {
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
    fn an_empty_first_page_makes_exactly_one_request() {
        let server = MockServer::start();
        let auth = auth_mock(&server);
        let graphql = graphql_mock(&server, page_body(0, 0));

        let page = client(&server)
            .test_runs(XrayIssueKind::Test, "10400", None, false, 100)
            .unwrap();

        auth.assert_calls(1);
        graphql.assert_calls(1);
        assert!(page.results.as_ref().unwrap().is_empty());
    }

    /// Runs `run-<first>` to `run-<first + count - 1>` as one page with the
    /// given total.
    fn runs_page(total: Option<i64>, first: usize, count: usize) -> serde_json::Value {
        serde_json::json!({"data": {"getTestRuns": {
            "total": total,
            "start": first,
            "limit": 100,
            "results": (first..first + count)
                .map(|number| serde_json::json!({"id": format!("run-{number}")}))
                .collect::<Vec<_>>()
        }}})
    }

    /// The exact request body of the default Test query for one page.
    fn test_page_request(start: usize, limit: usize) -> serde_json::Value {
        serde_json::json!({
            "query": DOCUMENTED_GET_TEST_RUNS_FOR_TEST_QUERY,
            "variables": {
                "testIssueIds": ["10400"],
                "statuses": null,
                "limit": limit,
                "start": start,
            },
        })
    }

    /// Mocks one page. The body must match exactly, so a request for a page
    /// the test did not plan finds no mock and fails with a 404 instead of
    /// paging on.
    fn page_mock(
        server: &MockServer,
        request: serde_json::Value,
        status: u16,
        then_body: serde_json::Value,
    ) -> httpmock::Mock<'_> {
        server.mock(|when, then| {
            when.method(POST)
                .path("/api/v2/graphql")
                .header("Authorization", format!("Bearer {TOKEN}"))
                .json_body(request);
            then.status(status).json_body(then_body);
        })
    }

    fn run_ids(page: &TestRunPage) -> Vec<String> {
        page.results
            .as_ref()
            .unwrap()
            .iter()
            .map(|run| run.as_ref().unwrap().id.clone().unwrap())
            .collect()
    }

    fn expected_run_ids(count: usize) -> Vec<String> {
        (0..count).map(|number| format!("run-{number}")).collect()
    }

    fn test_runs_with_limit(
        server: &MockServer,
        limit: usize,
    ) -> Result<TestRunPage, XrayClientError> {
        client(server).test_runs(XrayIssueKind::Test, "10400", None, false, limit)
    }

    #[test]
    fn paging_collects_the_limit_and_asks_only_for_the_remainder() {
        let server = MockServer::start();
        let auth = auth_mock(&server);
        let pages = [(0, 100, 100), (100, 100, 100), (200, 50, 50)].map(|(start, limit, count)| {
            page_mock(
                &server,
                test_page_request(start, limit),
                200,
                runs_page(Some(1000), start, count),
            )
        });

        let page = test_runs_with_limit(&server, 250).unwrap();

        auth.assert_calls(1);
        for mock in pages {
            mock.assert_calls(1);
        }
        assert_eq!(run_ids(&page), expected_run_ids(250));
        assert_eq!(page.total, Some(1000));
        assert_eq!(page.start, Some(0));
        assert_eq!(page.limit, Some(250));
    }

    #[test]
    fn a_limit_below_the_page_size_is_one_smaller_request() {
        let server = MockServer::start();
        let _auth = auth_mock(&server);
        let graphql = page_mock(
            &server,
            test_page_request(0, 30),
            200,
            runs_page(Some(1000), 0, 30),
        );

        let page = test_runs_with_limit(&server, 30).unwrap();

        graphql.assert_calls(1);
        assert_eq!(run_ids(&page), expected_run_ids(30));
        assert_eq!(page.limit, Some(30));
    }

    #[test]
    fn paging_stops_when_the_total_is_reached_before_the_limit() {
        let server = MockServer::start();
        let _auth = auth_mock(&server);
        let first = page_mock(
            &server,
            test_page_request(0, 100),
            200,
            runs_page(Some(150), 0, 100),
        );
        let second = page_mock(
            &server,
            test_page_request(100, 100),
            200,
            runs_page(Some(150), 100, 50),
        );

        let page = test_runs_with_limit(&server, 1000).unwrap();

        first.assert_calls(1);
        second.assert_calls(1);
        assert_eq!(run_ids(&page), expected_run_ids(150));
        assert_eq!(page.total, Some(150));
        assert_eq!(page.limit, Some(1000));
    }

    #[test]
    fn paging_stops_when_a_page_has_no_total() {
        let server = MockServer::start();
        let _auth = auth_mock(&server);
        let first = page_mock(
            &server,
            test_page_request(0, 100),
            200,
            runs_page(None, 0, 100),
        );

        let page = test_runs_with_limit(&server, 250).unwrap();

        first.assert_calls(1);
        assert_eq!(run_ids(&page), expected_run_ids(100));
        assert_eq!(page.total, None);
    }

    #[test]
    fn paging_stops_when_a_page_returns_no_runs() {
        let empty_pages = [
            runs_page(Some(1000), 100, 0),
            serde_json::json!({"data": {"getTestRuns": {"total": 1000, "results": null}}}),
        ];
        for empty_page in empty_pages {
            let server = MockServer::start();
            let _auth = auth_mock(&server);
            let first = page_mock(
                &server,
                test_page_request(0, 100),
                200,
                runs_page(Some(1000), 0, 100),
            );
            let second = page_mock(&server, test_page_request(100, 100), 200, empty_page);

            let page = test_runs_with_limit(&server, 250).unwrap();

            first.assert_calls(1);
            second.assert_calls(1);
            assert_eq!(run_ids(&page), expected_run_ids(100));
            assert_eq!(page.total, Some(1000));
        }
    }

    #[test]
    fn a_failed_second_page_fails_the_call() {
        let failures = [
            (500, serde_json::json!({}), "Xray API error (HTTP 500)"),
            (
                200,
                serde_json::json!({"errors": [{"message": "start out of range"}]}),
                "Xray GraphQL query failed",
            ),
        ];
        for (status, body, expected) in failures {
            let server = MockServer::start();
            let _auth = auth_mock(&server);
            let first = page_mock(
                &server,
                test_page_request(0, 100),
                200,
                runs_page(Some(1000), 0, 100),
            );
            let second = page_mock(&server, test_page_request(100, 100), status, body);

            let error = test_runs_with_limit(&server, 250).unwrap_err();

            first.assert_calls(1);
            second.assert_calls(1);
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn every_page_carries_the_filter_statuses_and_details_query() {
        let server = MockServer::start();
        let auth = auth_mock(&server);
        let pages = [(0, 100, 100), (100, 50, 50)].map(|(start, limit, count)| {
            page_mock(
                &server,
                serde_json::json!({
                    "query": DOCUMENTED_GET_TEST_RUNS_FOR_TEST_EXECUTION_DETAILS_QUERY,
                    "variables": {
                        "testExecIssueIds": ["10500"],
                        "statuses": ["FAILED", "TODO"],
                        "limit": limit,
                        "start": start,
                    },
                }),
                200,
                runs_page(Some(400), start, count),
            )
        });

        let page = client(&server)
            .test_runs(
                XrayIssueKind::TestExecution,
                "10500",
                Some(&["FAILED".to_owned(), "TODO".to_owned()]),
                true,
                150,
            )
            .unwrap();

        auth.assert_calls(1);
        for mock in pages {
            mock.assert_calls(1);
        }
        assert_eq!(run_ids(&page), expected_run_ids(150));
    }

    #[test]
    fn the_final_page_rules() {
        assert!(is_final_page(Some(1000), 100, 0));
        assert!(is_final_page(None, 0, 100));
        assert!(is_final_page(Some(150), 100, 50));
        assert!(is_final_page(Some(120), 100, 50));
        assert!(!is_final_page(Some(151), 100, 50));
        assert!(!is_final_page(Some(1000), 100, 100));
    }

    #[test]
    fn authentication_posts_json_credentials_and_parses_the_string_token() {
        // Covered end to end by `test_runs_authenticates_then_queries…`;
        // this test pins the auth request shape on its own so a broken
        // auth mock fails here with a clear message.
        let server = MockServer::start();
        let auth = auth_mock(&server);
        let graphql = graphql_mock(&server, page_body(0, 0));

        client(&server)
            .test_runs(XrayIssueKind::Test, "10400", None, false, 100)
            .unwrap();

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

        let error = client(&server)
            .test_runs(XrayIssueKind::Test, "10400", None, false, 100)
            .unwrap_err();

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

        let error = client(&server)
            .test_runs(XrayIssueKind::Test, "10400", None, false, 100)
            .unwrap_err();

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

        let error = client(&server)
            .test_runs(XrayIssueKind::Test, "10400", None, false, 100)
            .unwrap_err();

        assert!(matches!(error, XrayClientError::ResponseTooLarge));
    }

    #[test]
    fn a_graphql_response_without_data_is_invalid() {
        let server = MockServer::start();
        let _auth = auth_mock(&server);
        let _graphql = graphql_mock(&server, serde_json::json!({"data": null}));

        let error = client(&server)
            .test_runs(XrayIssueKind::Test, "10400", None, false, 100)
            .unwrap_err();

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

        let error = client(&server)
            .test_runs(XrayIssueKind::Test, "10400", None, false, 100)
            .unwrap_err();

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

        let error = client(&server)
            .test_runs(XrayIssueKind::Test, "10400", None, false, 100)
            .unwrap_err();
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

        let error = client(&server)
            .test_runs(XrayIssueKind::Test, "10400", None, false, 100)
            .unwrap_err();

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

            let error = client(&server)
                .test_runs(XrayIssueKind::Test, "10400", None, false, 100)
                .unwrap_err();
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

            let error = client(&server)
                .test_runs(XrayIssueKind::Test, "10400", None, false, 100)
                .unwrap_err();
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

    #[test]
    fn the_bounded_reader_rejects_more_bytes_than_the_limit_without_content_length() {
        let error = read_bounded(Cursor::new(vec![b'a'; 21]), 20).unwrap_err();
        assert!(matches!(error, XrayClientError::ResponseTooLarge));

        let exactly_at_limit = read_bounded(Cursor::new(vec![b'a'; 20]), 20).unwrap();
        assert_eq!(exactly_at_limit.len(), 20);
    }

    #[test]
    fn a_chunked_response_without_content_length_is_still_bounded() {
        // Raw TCP server answering with a chunked body of 256 bytes and no
        // Content-Length, which bypasses the fast header check entirely.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let writer = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            let mut response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
            for _ in 0..4 {
                response.extend_from_slice(b"40\r\n");
                response.extend_from_slice(&[b'a'; 64]);
                response.extend_from_slice(b"\r\n");
            }
            response.extend_from_slice(b"0\r\n\r\n");
            // The client hangs up once it passes the limit, so a write into
            // the closed socket is expected and must not fail the test.
            let _ = stream.write_all(&response);
        });

        let response = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
            .get(format!("http://{addr}/"))
            .send()
            .unwrap();
        let error = read_bounded_body(response, 100).unwrap_err();

        assert!(matches!(error, XrayClientError::ResponseTooLarge));
        writer.join().unwrap();
    }

    #[test]
    fn a_stalled_response_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        // Accept the connection and then say nothing at all. Detached on
        // purpose: the test must finish through the timeout, not the server.
        thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_secs(10));
        });
        let config = XrayConfig {
            base_url: Url::parse(&format!("http://{addr}/")).unwrap(),
            client_id: CLIENT_ID.to_owned(),
            client_secret: CLIENT_SECRET.to_owned(),
        };
        let client = XrayClient::with_timeouts(
            &config,
            Duration::from_millis(50),
            Duration::from_millis(400),
        )
        .unwrap();

        let started = std::time::Instant::now();
        let error = client
            .test_runs(XrayIssueKind::Test, "10400", None, false, 100)
            .unwrap_err();

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the stalled response took {:?} to fail",
            started.elapsed()
        );
        match error {
            XrayClientError::Http { source } => assert!(source.is_timeout(), "{source:?}"),
            other => panic!("expected a timeout, got {other:?}"),
        }
    }

    #[test]
    fn network_errors_never_leak_url_parts_or_the_client_secret() {
        // A port that is guaranteed closed: the bind below only exists to
        // reserve a free one, and dropping the listener closes it again.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let config = XrayConfig {
            base_url: Url::parse(&format!(
                "http://user:stolen@127.0.0.1:{port}/xray/?q=unsafe#frag"
            ))
            .unwrap(),
            client_id: CLIENT_ID.to_owned(),
            client_secret: CLIENT_SECRET.to_owned(),
        };
        let client = XrayClient::new(&config).unwrap();

        let error = client
            .test_runs(XrayIssueKind::Test, "10400", None, false, 100)
            .unwrap_err();

        assert!(matches!(error, XrayClientError::Http { .. }));
        let debug = format!("{error:?}");
        let display = error.to_string();
        let source_display = error
            .source()
            .map(std::string::ToString::to_string)
            .unwrap_or_default();
        for leaked in [
            "user:stolen",
            "stolen",
            "q=unsafe",
            "frag",
            CLIENT_SECRET,
            // The request URL itself: with the strip gone, reqwest puts the
            // full endpoint URL (path included) into Debug output. The bare
            // connect address of a failed TCP attempt is not the URL and may
            // legitimately remain in the source error.
            "/api/v2/authenticate",
        ] {
            assert!(!debug.contains(leaked), "Debug leaked {leaked}: {debug}");
            assert!(!display.contains(leaked), "Display leaked {leaked}");
            assert!(
                !source_display.contains(leaked),
                "source Display leaked {leaked}"
            );
        }
    }

    #[test]
    fn an_adversarial_status_travels_as_a_json_variable_only() {
        let server = MockServer::start();
        let _auth = auth_mock(&server);
        // Quotes, backslashes, a newline, and GraphQL-shaped text: if this
        // value were interpolated into the query instead of serialized as a
        // JSON variable, the body below could not match.
        let adversarial = r#"PASSED"}]
\ mutation { deleteTestRun } { getTestRuns(limit: 1) \" trailing"#;
        let expected_body = serde_json::json!({
            "query": DOCUMENTED_GET_TEST_RUNS_FOR_TEST_QUERY,
            "variables": {
                "testIssueIds": ["10400"],
                "statuses": [adversarial],
                "limit": 100,
                "start": 0,
            },
        });
        let _graphql = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v2/graphql")
                .json_body(expected_body)
                // Raw-body pin of the documented declaration, so the query
                // text on the wire is checked without reusing the constant.
                .body_includes(
                    "query GetTestRuns($testIssueIds: [String], $statuses: [String], $limit: Int!, $start: Int)",
                );
            then.status(200)
                .json_body(serde_json::json!({"errors": [{"message": adversarial}]}));
        });

        let error = client(&server)
            .test_runs(
                XrayIssueKind::Test,
                "10400",
                Some(&[adversarial.to_owned()]),
                false,
                100,
            )
            .unwrap_err();

        _graphql.assert_calls(1);
        assert_eq!(error.to_string(), "Xray GraphQL query failed");
        let debug = format!("{error:?}");
        assert!(!debug.contains("deleteTestRun"), "{debug}");
        assert!(!debug.contains("\\\"}]"), "{debug}");
    }
}
