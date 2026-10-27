mod common;

use common::jcli;
use httpmock::Method::GET;
use httpmock::MockServer;
use serde_json::Value;
use std::process::Output;

// base64 of "cli@example.com:cli-test-token"
const AUTH_HEADER: &str = "Basic Y2xpQGV4YW1wbGUuY29tOmNsaS10ZXN0LXRva2Vu";
const FIELDS: &str = "summary,status,issuetype,created,updated,labels";
const JQL: &str = r#"project = "ABC" AND text ~ "a&b+c" ORDER BY created DESC"#;

fn run(server: &MockServer, args: &[&str]) -> Output {
    jcli(
        args,
        &[
            ("JIRA_BASE_URL", server.base_url().as_str()),
            ("JIRA_EMAIL", "cli@example.com"),
            ("JIRA_API_TOKEN", "cli-test-token"),
        ],
    )
}

fn search(server: &MockServer) -> Output {
    run(server, &["issue", "search", "--jql", JQL, "--json"])
}

fn search_mock<'a>(
    server: &'a MockServer,
    max_results: &str,
    status: u16,
    body: Value,
) -> httpmock::Mock<'a> {
    let max_results = max_results.to_owned();
    server.mock(move |when, then| {
        when.method(GET)
            .path("/rest/api/3/search/jql")
            .query_param("jql", JQL)
            .query_param("maxResults", max_results)
            .query_param("fields", FIELDS)
            .query_param_count(r".*", r".*", 3)
            .header("Authorization", AUTH_HEADER);
        then.status(status).json_body(body);
    })
}

fn any_request(server: &MockServer) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.any_request();
        then.status(200);
    })
}

#[test]
fn prints_stable_json_in_jira_order() {
    let server = MockServer::start();
    let mock = search_mock(
        &server,
        "20",
        200,
        serde_json::json!({
            "issues": [
                {
                    "id": "10002",
                    "key": "ABC-2",
                    "fields": {
                        "summary": "Newer issue",
                        "issuetype": {"name": "Bug"},
                        "status": {"name": "Open"},
                        "created": "2026-10-17T02:13:45.123+0200",
                        "updated": "2026-10-17T03:00:00.000+0200",
                        "labels": ["backend"]
                    }
                },
                {
                    "id": "10001",
                    "key": "ABC-1",
                    "fields": {
                        "summary": "Older issue",
                        "issuetype": null,
                        "status": {"name": "Done"},
                        "created": "2026-10-16T09:00:00.000+0200",
                        "updated": null,
                        "labels": []
                    }
                }
            ],
            "isLast": false
        }),
    );

    let output = search(&server);

    mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        r#"{
  "jql": "project = \"ABC\" AND text ~ \"a&b+c\" ORDER BY created DESC",
  "returned": 2,
  "truncated": true,
  "issues": [
    {
      "key": "ABC-2",
      "issueType": "Bug",
      "status": "Open",
      "summary": "Newer issue",
      "created": "2026-10-17T02:13:45.123+0200",
      "updated": "2026-10-17T03:00:00.000+0200",
      "labels": [
        "backend"
      ]
    },
    {
      "key": "ABC-1",
      "issueType": null,
      "status": "Done",
      "summary": "Older issue",
      "created": "2026-10-16T09:00:00.000+0200",
      "updated": null,
      "labels": []
    }
  ]
}
"#
    );
}

#[test]
fn an_empty_result_exits_zero_with_no_issues() {
    let server = MockServer::start();
    let mock = search_mock(
        &server,
        "20",
        200,
        serde_json::json!({"issues": [], "isLast": true}),
    );

    let output = search(&server);

    mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["returned"], 0);
    assert_eq!(value["truncated"], false);
    assert_eq!(value["issues"], serde_json::json!([]));
}

#[test]
fn a_missing_is_last_makes_truncation_null() {
    let server = MockServer::start();
    let _mock = search_mock(
        &server,
        "20",
        200,
        serde_json::json!({"issues": [{"key": "ABC-1", "fields": {}}]}),
    );

    let output = search(&server);

    assert_eq!(output.status.code(), Some(0));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["returned"], 1);
    assert!(value["truncated"].is_null());
}

#[test]
fn the_limit_bounds_are_accepted_and_one_page_asks_for_at_most_100() {
    for (limit, max_results) in [("1", "1"), ("100", "100"), ("1000", "100")] {
        let server = MockServer::start();
        let mock = search_mock(
            &server,
            max_results,
            200,
            serde_json::json!({"issues": [], "isLast": true}),
        );

        let output = run(
            &server,
            &["issue", "search", "--jql", JQL, "--json", "--limit", limit],
        );

        mock.assert_calls(1);
        assert_eq!(output.status.code(), Some(0), "limit {limit}");
    }
}

#[test]
fn limits_outside_the_range_are_rejected_before_any_request() {
    for limit in ["0", "1001", "-1", "ten"] {
        let server = MockServer::start();
        let anything = any_request(&server);
        // The `=` form hands `-1` to the value parser instead of reading it
        // as another flag.
        let limit_arg = format!("--limit={limit}");

        let output = run(
            &server,
            &["issue", "search", "--jql", JQL, "--json", &limit_arg],
        );

        anything.assert_calls(0);
        assert_eq!(output.status.code(), Some(2), "limit {limit}");
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("--limit"), "limit {limit}: {stderr}");
    }
}

#[test]
fn the_json_flag_is_required() {
    let server = MockServer::start();
    let anything = any_request(&server);

    let output = run(&server, &["issue", "search", "--jql", JQL]);

    anything.assert_calls(0);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--json"), "stderr: {stderr}");
}

#[test]
fn the_jql_flag_is_required() {
    let server = MockServer::start();
    let anything = any_request(&server);

    let output = run(&server, &["issue", "search", "--json"]);

    anything.assert_calls(0);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--jql"), "stderr: {stderr}");
}

#[test]
fn missing_configuration_exits_two_naming_the_variable() {
    let output = jcli(&["issue", "search", "--jql", JQL, "--json"], &[]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("JIRA_BASE_URL"), "stderr: {stderr}");
}

#[test]
fn invalid_jql_reports_jira_messages_on_one_sanitized_line() {
    let server = MockServer::start();
    let mock = search_mock(
        &server,
        "20",
        400,
        serde_json::json!({
            "errorMessages": [
                "Field 'bogus' does not exist\r\nor you lack permission\u{1b}[31m\u{202e}."
            ],
            "errors": {"jql": "Expected a value"}
        }),
    );

    let output = search(&server);

    mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: invalid JQL query (400): Field 'bogus' does not exist or you lack permission[31m.; jql: Expected a value\n"
    );
}

#[test]
fn a_400_without_messages_reports_the_generic_status() {
    let server = MockServer::start();
    let mock = search_mock(
        &server,
        "20",
        400,
        serde_json::json!({"unexpected": "cli-test-token"}),
    );

    let output = search(&server);

    mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: Jira API error (HTTP 400)\n"
    );
}

#[test]
fn other_jira_errors_exit_three_without_response_data() {
    let server = MockServer::start();
    let mock = search_mock(
        &server,
        "20",
        401,
        serde_json::json!({"errorMessages": ["cli-test-token cli@example.com"]}),
    );

    let output = search(&server);

    mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: authentication failed (401): check JIRA_EMAIL and JIRA_API_TOKEN\n"
    );
}

#[test]
fn output_escapes_terminal_controls_in_jira_values() {
    let server = MockServer::start();
    let _mock = search_mock(
        &server,
        "20",
        200,
        serde_json::json!({
            "issues": [{"key": "ABC-1", "fields": {"summary": "Fix\u{202e}\u{9b}31m"}}],
            "isLast": true
        }),
    );

    let output = search(&server);

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("\"summary\": \"Fix\\u202E\\u009B31m\""),
        "{stdout}"
    );
    assert!(!stdout.contains('\u{202e}'));
    assert!(!stdout.contains('\u{9b}'));
}

fn next_page_mock<'a>(
    server: &'a MockServer,
    max_results: &str,
    token: &str,
    status: u16,
    body: Value,
) -> httpmock::Mock<'a> {
    let max_results = max_results.to_owned();
    let token = token.to_owned();
    server.mock(move |when, then| {
        when.method(GET)
            .path("/rest/api/3/search/jql")
            .query_param("jql", JQL)
            .query_param("maxResults", max_results)
            .query_param("fields", FIELDS)
            .query_param("nextPageToken", token)
            .query_param_count(r".*", r".*", 4)
            .header("Authorization", AUTH_HEADER);
        then.status(status).json_body(body);
    })
}

fn search_with_limit(server: &MockServer, limit: &str) -> Output {
    run(
        server,
        &["issue", "search", "--jql", JQL, "--json", "--limit", limit],
    )
}

fn first_of_two_pages(server: &MockServer) -> httpmock::Mock<'_> {
    search_mock(
        server,
        "3",
        200,
        serde_json::json!({
            "issues": [
                {"key": "ABC-5", "fields": {"summary": "Fifth", "labels": ["a"]}},
                {"key": "ABC-4", "fields": {"summary": "Fourth"}}
            ],
            "nextPageToken": "page-2",
            "isLast": false
        }),
    )
}

#[test]
fn follows_the_page_token_until_the_limit_and_keeps_jira_order() {
    let server = MockServer::start();
    let first = first_of_two_pages(&server);
    let second = next_page_mock(
        &server,
        "1",
        "page-2",
        200,
        serde_json::json!({
            "issues": [{"key": "ABC-3", "fields": {"summary": "Third"}}],
            "nextPageToken": "page-3",
            "isLast": false
        }),
    );

    let output = search_with_limit(&server, "3");

    first.assert_calls(1);
    second.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        r#"{
  "jql": "project = \"ABC\" AND text ~ \"a&b+c\" ORDER BY created DESC",
  "returned": 3,
  "truncated": true,
  "issues": [
    {
      "key": "ABC-5",
      "issueType": null,
      "status": null,
      "summary": "Fifth",
      "created": null,
      "updated": null,
      "labels": [
        "a"
      ]
    },
    {
      "key": "ABC-4",
      "issueType": null,
      "status": null,
      "summary": "Fourth",
      "created": null,
      "updated": null,
      "labels": []
    },
    {
      "key": "ABC-3",
      "issueType": null,
      "status": null,
      "summary": "Third",
      "created": null,
      "updated": null,
      "labels": []
    }
  ]
}
"#
    );
}

#[test]
fn the_last_page_before_the_limit_is_not_truncated() {
    let server = MockServer::start();
    let first = first_of_two_pages(&server);
    let second = next_page_mock(
        &server,
        "1",
        "page-2",
        200,
        serde_json::json!({"issues": [{"key": "ABC-3"}], "isLast": true}),
    );

    let output = search_with_limit(&server, "3");

    first.assert_calls(1);
    second.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["returned"], 3);
    assert_eq!(value["truncated"], false);
}

#[test]
fn a_failed_second_page_fails_with_empty_stdout() {
    let server = MockServer::start();
    let first = first_of_two_pages(&server);
    let second = next_page_mock(
        &server,
        "1",
        "page-2",
        500,
        serde_json::json!({"errorMessages": ["cli-test-token"]}),
    );

    let output = search_with_limit(&server, "3");

    first.assert_calls(1);
    second.assert_calls(1);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: Jira API error (HTTP 500)\n"
    );
}
