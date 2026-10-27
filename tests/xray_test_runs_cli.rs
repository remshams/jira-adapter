mod common;

use common::jcli;
use httpmock::Method::{GET, POST};
use httpmock::MockServer;
use serde_json::Value;

const JIRA_AUTH_HEADER: &str = "Basic Y2xpQGV4YW1wbGUuY29tOmNsaS10ZXN0LXRva2Vu";
const XRAY_CLIENT_ID: &str = "xray-client-id";
const XRAY_CLIENT_SECRET: &str = "xray-client-secret";
const XRAY_TOKEN: &str = "xray-bearer-token";

fn xray_env(server: &MockServer) -> Vec<(&'static str, String)> {
    vec![
        ("XRAY_BASE_URL", server.base_url()),
        ("XRAY_CLIENT_ID", XRAY_CLIENT_ID.to_owned()),
        ("XRAY_CLIENT_SECRET", XRAY_CLIENT_SECRET.to_owned()),
    ]
}

fn run(server: &MockServer, extra_env: &[(&str, String)]) -> std::process::Output {
    run_args(
        server,
        &["xray", "test-runs", "TEST-123", "--json"],
        extra_env,
    )
}

/// Runs `jcli` with the full standard environment and extra CLI arguments.
fn run_args(
    server: &MockServer,
    args: &[&str],
    extra_env: &[(&str, String)],
) -> std::process::Output {
    let args: Vec<&str> = args.to_vec();
    let mut env: Vec<(&str, String)> = vec![
        ("JIRA_BASE_URL", server.base_url()),
        ("JIRA_EMAIL", "cli@example.com".to_owned()),
        ("JIRA_API_TOKEN", "cli-test-token".to_owned()),
    ];
    env.extend(xray_env(server));
    env.extend(extra_env.iter().cloned());
    let env_refs: Vec<(&str, &str)> = env
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    jcli(&args, &env_refs)
}

fn mock_jira_issue(server: &MockServer) -> httpmock::Mock<'_> {
    mock_jira_issue_with_fields(
        server,
        serde_json::json!({"issuetype": {"id": "10010", "name": "Test"}}),
    )
}

/// Mocks the issue lookup for TEST-123 (ID 10400), which must ask Jira for
/// the `issuetype` field only, and answers with the given `fields` object.
fn mock_jira_issue_with_fields(
    server: &MockServer,
    fields: serde_json::Value,
) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.method(GET)
            .path("/rest/api/3/issue/TEST-123")
            .query_param("fields", "issuetype")
            .query_param_count(r".*", r".*", 1)
            .header("Authorization", JIRA_AUTH_HEADER);
        then.status(200).json_body(serde_json::json!({
            "id": "10400",
            "key": "TEST-123",
            "fields": fields
        }));
    })
}

/// Mocks the Test Execution query: the issue ID travels in
/// `testExecIssueIds`, and the body carries no `testIssueIds` filter.
fn mock_graphql_for_test_execution<'a>(
    server: &'a MockServer,
    body: serde_json::Value,
    statuses: Option<&'a [&'a str]>,
) -> httpmock::Mock<'a> {
    let variables = serde_json::json!({"variables": {
        "testExecIssueIds": ["10400"],
        "statuses": statuses,
        "limit": 100,
        "start": 0
    }});
    server.mock(|when, then| {
        when.method(POST)
            .path("/api/v2/graphql")
            .header("Authorization", format!("Bearer {XRAY_TOKEN}"))
            .json_body_includes(variables.to_string())
            .body_includes("getTestRuns(testExecIssueIds: $testExecIssueIds, statuses: $statuses")
            .body_excludes("testIssueIds");
        then.status(200).json_body(body);
    })
}

/// Matches any request on the Xray endpoints, to prove none was sent.
fn mock_any_xray_request(server: &MockServer) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.method(POST).path_matches(r"^/api/v2/");
        then.status(500);
    })
}

fn mock_auth(server: &MockServer) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.method(POST)
            .path("/api/v2/authenticate")
            .json_body(serde_json::json!({
                "client_id": XRAY_CLIENT_ID,
                "client_secret": XRAY_CLIENT_SECRET,
            }));
        then.status(200).body(format!("\"{XRAY_TOKEN}\""));
    })
}

fn mock_graphql(server: &MockServer, body: serde_json::Value) -> httpmock::Mock<'_> {
    mock_graphql_with_statuses(server, body, None)
}

/// Mocks a GraphQL request that must carry the given statuses in the
/// `variables` object, in this exact order and spelling. `None` pins the
/// absent case as JSON null.
fn mock_graphql_with_statuses<'a>(
    server: &'a MockServer,
    body: serde_json::Value,
    statuses: Option<&'a [&'a str]>,
) -> httpmock::Mock<'a> {
    let statuses_json = statuses.map_or_else(
        || "null".to_owned(),
        |statuses| serde_json::to_string(statuses).unwrap(),
    );
    let variables = format!(
        r#"{{"variables": {{"testIssueIds": ["10400"], "limit": 100, "start": 0, "statuses": {statuses_json}}}}}"#
    );
    server.mock(|when, then| {
        when.method(POST)
            .path("/api/v2/graphql")
            .header("Authorization", format!("Bearer {XRAY_TOKEN}"))
            .json_body_includes(variables.as_str())
            .body_includes("getTestRuns(testIssueIds: $testIssueIds, statuses: $statuses")
            .body_excludes("testExecIssueIds");
        then.status(200).json_body(body);
    })
}

fn page_body(total: i64, result_count: usize) -> serde_json::Value {
    serde_json::json!({
        "data": {"getTestRuns": {
            "total": total,
            "start": 0,
            "limit": 100,
            "results": (0..result_count).map(|index| serde_json::json!({
                "id": format!("run-{index}"),
                "status": {"name": "PASSED"},
                "startedOn": if index == 0 { serde_json::json!("2024-05-01T08:00:00+02:00") } else { serde_json::Value::Null },
                "finishedOn": serde_json::Value::Null,
                "testExecution": if index == 0 { serde_json::json!({"issueId": "10500"}) } else { serde_json::Value::Null }
            })).collect::<Vec<_>>()
        }}
    })
}

fn run_entry(id: &str, started_on: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "status": {"name": "PASSED"},
        "startedOn": started_on,
        "finishedOn": serde_json::Value::Null,
        "testExecution": serde_json::Value::Null
    })
}

fn results_page(results: serde_json::Value) -> serde_json::Value {
    let total = results.as_array().map_or(0, Vec::len) as i64;
    serde_json::json!({"data": {"getTestRuns": {
        "total": total,
        "start": 0,
        "limit": 100,
        "results": results
    }}})
}

fn output_ids(stdout: &str) -> Vec<Option<String>> {
    let value: Value = serde_json::from_str(stdout).unwrap();
    value["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| {
            if run.is_null() {
                None
            } else {
                run["id"].as_str().map(str::to_owned)
            }
        })
        .collect()
}

#[test]
fn prints_stable_json_for_the_first_page() {
    let server = MockServer::start();
    let jira_issue = mock_jira_issue(&server);
    let auth = mock_auth(&server);
    let graphql = mock_graphql(&server, page_body(2, 2));

    let output = run(&server, &[]);

    jira_issue.assert_calls(1);
    auth.assert_calls(1);
    graphql.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());

    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["issueKey"], "TEST-123");
    assert_eq!(value["issueKind"], "test");
    assert_eq!(value["total"], 2);
    assert_eq!(value["returned"], 2);
    assert_eq!(value["truncated"], false);
    assert_eq!(value["start"], 0);
    assert_eq!(value["limit"], 100);
    assert_eq!(value["runs"][0]["id"], "run-0");
    assert_eq!(value["runs"][0]["status"], "PASSED");
    assert_eq!(value["runs"][0]["startedOn"], "2024-05-01T08:00:00+02:00");
    assert_eq!(value["runs"][0]["finishedOn"], Value::Null);
    assert_eq!(value["runs"][0]["testExecutionIssueId"], "10500");
    assert_eq!(value["runs"][1]["status"], "PASSED");
    assert_eq!(value["runs"][1]["startedOn"], Value::Null);
    assert_eq!(value["runs"][1]["testExecutionIssueId"], Value::Null);
    assert!(stdout.ends_with("}\n"));
    assert!(!stdout.contains(XRAY_CLIENT_SECRET));
    assert!(!stdout.contains(XRAY_TOKEN));
}

#[test]
fn by_default_one_page_of_100_runs_is_read_and_more_are_truncated() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue(&server);
    let _auth = mock_auth(&server);
    let graphql = mock_graphql(&server, page_body(250, 100));

    let output = run(&server, &[]);

    graphql.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    let value: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    assert_eq!(value["total"], 250);
    assert_eq!(value["returned"], 100);
    assert_eq!(value["truncated"], true);
    assert_eq!(value["start"], 0);
    assert_eq!(value["limit"], 100);
}

/// Mocks the page of TEST-123's runs at `start` with `limit`. The variables
/// must match, so a request for a page the test did not plan fails.
fn mock_page(
    server: &MockServer,
    start: usize,
    limit: usize,
    status: u16,
    body: serde_json::Value,
) -> httpmock::Mock<'_> {
    let variables = serde_json::json!({"variables": {
        "testIssueIds": ["10400"],
        "statuses": null,
        "limit": limit,
        "start": start
    }});
    server.mock(|when, then| {
        when.method(POST)
            .path("/api/v2/graphql")
            .header("Authorization", format!("Bearer {XRAY_TOKEN}"))
            .json_body_includes(variables.to_string())
            .body_includes("getTestRuns(testIssueIds: $testIssueIds, statuses: $statuses");
        then.status(status).json_body(body);
    })
}

/// Runs `run-<first>` onwards as one page; run N started N minutes after
/// midnight, so later pages hold newer runs.
fn numbered_page(total: i64, first: usize, count: usize) -> serde_json::Value {
    let results: Vec<serde_json::Value> = (first..first + count)
        .map(|number| {
            let started_on = format!("2024-05-01T{:02}:{:02}:00Z", number / 60, number % 60);
            run_entry(&format!("run-{number}"), serde_json::json!(started_on))
        })
        .collect();
    serde_json::json!({"data": {"getTestRuns": {
        "total": total,
        "start": first,
        "limit": 100,
        "results": results
    }}})
}

#[test]
fn limit_reads_runs_across_two_pages() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue(&server);
    let auth = mock_auth(&server);
    let first = mock_page(&server, 0, 100, 200, numbered_page(400, 0, 100));
    let second = mock_page(&server, 100, 50, 200, numbered_page(400, 100, 50));

    let output = run_args(
        &server,
        &["xray", "test-runs", "TEST-123", "--json", "--limit", "150"],
        &[],
    );

    auth.assert_calls(1);
    first.assert_calls(1);
    second.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["total"], 400);
    assert_eq!(value["returned"], 150);
    assert_eq!(value["truncated"], true);
    assert_eq!(value["start"], 0);
    assert_eq!(value["limit"], 150);
    let expected: Vec<Option<String>> = (0..150).map(|n| Some(format!("run-{n}"))).collect();
    assert_eq!(output_ids(&stdout), expected);
}

#[test]
fn paging_stops_at_the_total_and_the_output_is_not_truncated() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue(&server);
    let _auth = mock_auth(&server);
    let first = mock_page(&server, 0, 100, 200, numbered_page(120, 0, 100));
    let second = mock_page(&server, 100, 100, 200, numbered_page(120, 100, 20));

    let output = run_args(
        &server,
        &["xray", "test-runs", "TEST-123", "--json", "--limit", "1000"],
        &[],
    );

    first.assert_calls(1);
    second.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    let value: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    assert_eq!(value["total"], 120);
    assert_eq!(value["returned"], 120);
    assert_eq!(value["truncated"], false);
    assert_eq!(value["limit"], 1000);
}

#[test]
fn sort_orders_the_runs_of_all_pages() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue(&server);
    let _auth = mock_auth(&server);
    let _first = mock_page(&server, 0, 100, 200, numbered_page(130, 0, 100));
    let _second = mock_page(&server, 100, 100, 200, numbered_page(130, 100, 30));

    let output = run_args(
        &server,
        &[
            "xray",
            "test-runs",
            "TEST-123",
            "--json",
            "--limit",
            "200",
            "--sort",
            "started-on",
        ],
        &[],
    );

    assert_eq!(output.status.code(), Some(0));
    let ids = output_ids(&String::from_utf8(output.stdout).unwrap());
    let expected: Vec<Option<String>> = (0..130).rev().map(|n| Some(format!("run-{n}"))).collect();
    assert_eq!(ids, expected);
}

#[test]
fn a_failed_second_page_exits_three_with_empty_stdout() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue(&server);
    let _auth = mock_auth(&server);
    let first = mock_page(&server, 0, 100, 200, numbered_page(400, 0, 100));
    let second = mock_page(&server, 100, 100, 500, serde_json::json!({}));

    let output = run_args(
        &server,
        &["xray", "test-runs", "TEST-123", "--json", "--limit", "200"],
        &[],
    );

    first.assert_calls(1);
    second.assert_calls(1);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr, "error: Xray API error (HTTP 500)\n");
}

#[test]
fn limits_outside_the_range_are_rejected_before_any_request() {
    for limit in ["0", "1001", "-1", "ten"] {
        let server = MockServer::start();
        let anything = server.mock(|when, then| {
            when.any_request();
            then.status(200);
        });
        let limit_arg = format!("--limit={limit}");

        let output = run_args(
            &server,
            &["xray", "test-runs", "TEST-123", "--json", &limit_arg],
            &[],
        );

        assert_eq!(output.status.code(), Some(2), "limit {limit}");
        assert!(output.stdout.is_empty(), "limit {limit}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("--limit"), "limit {limit}: {stderr}");
        anything.assert_calls(0);
    }
}

#[test]
fn a_test_execution_key_queries_the_runs_it_contains() {
    let server = MockServer::start();
    let jira_issue = mock_jira_issue_with_fields(
        &server,
        serde_json::json!({"issuetype": {"id": "10011", "name": "Test Execution"}}),
    );
    let auth = mock_auth(&server);
    let graphql = mock_graphql_for_test_execution(&server, page_body(2, 2), None);

    let output = run(&server, &[]);

    jira_issue.assert_calls(1);
    auth.assert_calls(1);
    graphql.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    assert_eq!(value["issueKey"], "TEST-123");
    assert_eq!(value["issueKind"], "testExecution");
    assert_eq!(value["total"], 2);
    assert_eq!(value["returned"], 2);
    assert_eq!(value["runs"][0]["id"], "run-0");
}

#[test]
fn a_test_execution_key_without_runs_prints_an_empty_list() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue_with_fields(
        &server,
        serde_json::json!({"issuetype": {"name": "Test Execution"}}),
    );
    let _auth = mock_auth(&server);
    let graphql = mock_graphql_for_test_execution(&server, page_body(0, 0), None);

    let output = run(&server, &[]);

    graphql.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    let value: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    assert_eq!(value["issueKind"], "testExecution");
    assert_eq!(value["total"], 0);
    assert_eq!(value["runs"], serde_json::json!([]));
}

#[test]
fn a_sub_test_execution_key_queries_the_runs_it_contains() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue_with_fields(
        &server,
        serde_json::json!({"issuetype": {"name": "Sub Test Execution"}}),
    );
    let _auth = mock_auth(&server);
    let graphql = mock_graphql_for_test_execution(&server, page_body(1, 1), None);

    let output = run(&server, &[]);

    graphql.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    let value: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    assert_eq!(value["issueKind"], "testExecution");
    assert_eq!(value["returned"], 1);
}

#[test]
fn status_and_sort_flags_work_for_a_test_execution_key() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue_with_fields(
        &server,
        serde_json::json!({"issuetype": {"name": "Test Execution"}}),
    );
    let _auth = mock_auth(&server);
    let graphql = mock_graphql_for_test_execution(
        &server,
        results_page(serde_json::json!([
            run_entry("run-old", "2024-05-01T12:00:00Z".into()),
            run_entry("run-new", "2024-05-02T12:00:00Z".into())
        ])),
        Some(&["FAILED"]),
    );

    let output = run_args(
        &server,
        &[
            "xray",
            "test-runs",
            "TEST-123",
            "--json",
            "--status",
            "FAILED",
            "--sort",
            "started-on",
            "--direction",
            "asc",
        ],
        &[],
    );

    graphql.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        output_ids(&stdout),
        [Some("run-old".to_owned()), Some("run-new".to_owned())]
    );
}

#[test]
fn another_issue_type_exits_three_without_an_xray_request() {
    let server = MockServer::start();
    let jira_issue =
        mock_jira_issue_with_fields(&server, serde_json::json!({"issuetype": {"name": "Story"}}));
    let xray = mock_any_xray_request(&server);

    let output = run(&server, &[]);

    jira_issue.assert_calls(1);
    xray.assert_calls(0);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: TEST-123 is not an Xray Test or Test Execution (issue type: Story)\n"
    );
}

#[test]
fn type_names_match_case_sensitively() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue_with_fields(
        &server,
        serde_json::json!({"issuetype": {"name": "test execution"}}),
    );
    let xray = mock_any_xray_request(&server);

    let output = run(&server, &[]);

    xray.assert_calls(0);
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: TEST-123 is not an Xray Test or Test Execution (issue type: test execution)\n"
    );
}

#[test]
fn a_missing_issue_type_exits_three_without_an_xray_request() {
    for fields in [
        serde_json::json!({}),
        serde_json::json!({"issuetype": null}),
        serde_json::json!({"issuetype": {"name": ""}}),
    ] {
        let server = MockServer::start();
        let _jira_issue = mock_jira_issue_with_fields(&server, fields.clone());
        let xray = mock_any_xray_request(&server);

        let output = run(&server, &[]);

        xray.assert_calls(0);
        assert_eq!(output.status.code(), Some(3), "{fields}");
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "error: TEST-123 is not an Xray Test or Test Execution (issue type: unknown)\n",
            "{fields}"
        );
    }
}

#[test]
fn an_issue_type_with_control_characters_is_printed_on_one_safe_line() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue_with_fields(
        &server,
        serde_json::json!({"issuetype": {"name": "Epic\n\u{1b}[31mspoof\u{202E}"}}),
    );
    let _xray = mock_any_xray_request(&server);

    let output = run(&server, &[]);

    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: TEST-123 is not an Xray Test or Test Execution (issue type: Epic [31mspoof)\n"
    );
}

#[test]
fn missing_xray_credentials_exit_two_before_any_request() {
    let server = MockServer::start();
    let jira_issue = mock_jira_issue(&server);
    let auth = mock_auth(&server);
    let graphql = mock_graphql(&server, page_body(0, 0));
    let env: Vec<(&str, String)> = vec![
        ("JIRA_BASE_URL", server.base_url()),
        ("JIRA_EMAIL", "cli@example.com".to_owned()),
        ("JIRA_API_TOKEN", "cli-test-token".to_owned()),
        ("XRAY_BASE_URL", server.base_url()),
        ("XRAY_CLIENT_SECRET", XRAY_CLIENT_SECRET.to_owned()),
    ];
    let env_refs: Vec<(&str, &str)> = env
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();

    let output = jcli(&["xray", "test-runs", "TEST-123", "--json"], &env_refs);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("XRAY_CLIENT_ID"), "stderr: {stderr}");
    jira_issue.assert_calls(0);
    auth.assert_calls(0);
    graphql.assert_calls(0);
}

#[test]
fn missing_jira_credentials_are_reported_by_variable_name() {
    let server = MockServer::start();
    let env: Vec<(&str, String)> = xray_env(&server);
    let env_refs: Vec<(&str, &str)> = env
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();

    let output = jcli(&["xray", "test-runs", "TEST-123", "--json"], &env_refs);

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("JIRA_BASE_URL"), "stderr: {stderr}");
}

#[test]
fn xray_settings_are_not_required_for_jira_commands() {
    let server = MockServer::start();
    let jira_issue = server.mock(|when, then| {
        when.method(GET)
            .path("/rest/api/3/issue/ABC-123")
            .header("Authorization", JIRA_AUTH_HEADER);
        then.status(200).json_body(serde_json::json!({
            "id": "10400",
            "key": "ABC-123",
            "fields": {"summary": "Fix it", "status": {"name": "Open"}}
        }));
    });

    let output = jcli(
        &["issue", "read", "ABC-123"],
        &[
            ("JIRA_BASE_URL", server.base_url().as_str()),
            ("JIRA_EMAIL", "cli@example.com"),
            ("JIRA_API_TOKEN", "cli-test-token"),
        ],
    );

    jira_issue.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn the_json_flag_is_required() {
    let server = MockServer::start();

    let output = jcli(
        &["xray", "test-runs", "TEST-123"],
        &[
            ("JIRA_BASE_URL", server.base_url().as_str()),
            ("JIRA_EMAIL", "cli@example.com"),
            ("JIRA_API_TOKEN", "cli-test-token"),
            ("XRAY_BASE_URL", server.base_url().as_str()),
            ("XRAY_CLIENT_ID", XRAY_CLIENT_ID),
            ("XRAY_CLIENT_SECRET", XRAY_CLIENT_SECRET),
        ],
    );

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--json"), "stderr: {stderr}");
}

#[test]
fn an_invalid_issue_key_exits_one_without_any_request() {
    let server = MockServer::start();
    let anything = server.mock(|when, then| {
        when.method(GET);
        then.status(200);
    });

    let output = jcli(
        &[
            "xray",
            "test-runs",
            "https://example.atlassian.net/browse/AB-1",
            "--json",
        ],
        &[
            ("JIRA_BASE_URL", server.base_url().as_str()),
            ("JIRA_EMAIL", "cli@example.com"),
            ("JIRA_API_TOKEN", "cli-test-token"),
            ("XRAY_BASE_URL", server.base_url().as_str()),
            ("XRAY_CLIENT_ID", XRAY_CLIENT_ID),
            ("XRAY_CLIENT_SECRET", XRAY_CLIENT_SECRET),
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    anything.assert_calls(0);
}

#[test]
fn xray_http_failure_exits_three_without_leaking_response_data() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue(&server);
    let _auth = mock_auth(&server);
    server.mock(|when, then| {
        when.method(POST).path("/api/v2/graphql");
        then.status(500).body(format!(
            "{XRAY_CLIENT_SECRET} {XRAY_TOKEN}\n\u{1b}[31muntrusted"
        ));
    });

    let output = run(&server, &[]);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr, "error: Xray API error (HTTP 500)\n");
}

#[test]
fn graphql_errors_exit_three_without_stdout() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue(&server);
    let _auth = mock_auth(&server);
    let _graphql = mock_graphql(
        &server,
        serde_json::json!({"errors": [{"message": "Internal error"}]}),
    );

    let output = run(&server, &[]);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr, "error: Xray GraphQL query failed\n");
}

#[test]
fn graphql_error_messages_never_reach_stderr() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue(&server);
    let _auth = mock_auth(&server);
    let remote_message = format!(
        "{XRAY_CLIENT_SECRET} {XRAY_TOKEN} \u{1b}[31m line-one\r\nline-two\
         \u{061C}\u{200E}\u{200F}\u{202A}\u{202B}\u{202C}\
         \u{202D}\u{202E}\u{2066}\u{2067}\u{2068}\u{2069}\
         plus a substantial amount of untrusted server text"
    );
    let _graphql = mock_graphql(
        &server,
        serde_json::json!({"errors": [{"message": remote_message}]}),
    );

    let output = run(&server, &[]);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    let rendered = String::from_utf8(output.stderr).unwrap();
    assert_eq!(rendered, "error: Xray GraphQL query failed\n");
    for leaked in [XRAY_CLIENT_SECRET, XRAY_TOKEN, "line-one", "untrusted"] {
        assert!(!rendered.contains(leaked), "stderr leaked {leaked:?}");
    }
    for leaked in [
        '\u{1b}', '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}',
        '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
    ] {
        assert!(!rendered.contains(leaked), "stderr leaked {leaked:?}");
    }
}

#[test]
fn flat_config_file_keys_supply_jira_and_xray_values() {
    let server = MockServer::start();
    let config_dir = tempfile::tempdir().unwrap();
    let config_path = config_dir.path().join("jcli").join("config.toml");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(
        &config_path,
        format!(
            "base_url = \"{}\"\nemail = \"cli@example.com\"\napi_token = \"cli-test-token\"\n\
             xray_base_url = \"{}\"\nxray_client_id = \"{XRAY_CLIENT_ID}\"\n\
             xray_client_secret = \"{XRAY_CLIENT_SECRET}\"\n",
            server.base_url(),
            server.base_url()
        ),
    )
    .unwrap();

    let jira_issue = mock_jira_issue(&server);
    let auth = mock_auth(&server);
    let graphql = mock_graphql(&server, page_body(0, 0));

    let output = jcli(
        &["xray", "test-runs", "TEST-123", "--json"],
        &[("XDG_CONFIG_HOME", config_dir.path().to_str().unwrap())],
    );

    jira_issue.assert_calls(1);
    auth.assert_calls(1);
    graphql.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    let value: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    assert_eq!(value["issueKey"], "TEST-123");
}

#[test]
fn secrets_never_reach_stdout_or_stderr_on_the_happy_path() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue(&server);
    let _auth = mock_auth(&server);
    let _graphql = mock_graphql(&server, page_body(1, 1));

    let output = run(&server, &[]);

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    for secret in [XRAY_CLIENT_SECRET, XRAY_TOKEN, "cli-test-token"] {
        assert!(!stdout.contains(secret), "stdout leaked {secret}");
        assert!(!stderr.contains(secret), "stderr leaked {secret}");
    }
}

#[test]
fn repeated_status_flags_are_forwarded_unchanged_and_in_order() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue(&server);
    let _auth = mock_auth(&server);
    let _graphql =
        mock_graphql_with_statuses(&server, page_body(2, 2), Some(&["FAILED", "CUSTOM-42"]));

    let output = run_args(
        &server,
        &[
            "xray",
            "test-runs",
            "TEST-123",
            "--json",
            "--status",
            "FAILED",
            "--status",
            "CUSTOM-42",
        ],
        &[],
    );

    assert_eq!(output.status.code(), Some(0));
    _jira_issue.assert_calls(1);
    _auth.assert_calls(1);
    _graphql.assert_calls(1);
    let value: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    assert_eq!(value["runs"][0]["status"], "PASSED");
    assert_eq!(value["returned"], 2);
}

#[test]
fn a_single_status_flag_is_forwarded_as_one_value() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue(&server);
    let _auth = mock_auth(&server);
    let _graphql = mock_graphql_with_statuses(&server, page_body(1, 1), Some(&["PASSED"]));

    let output = run_args(
        &server,
        &[
            "xray",
            "test-runs",
            "TEST-123",
            "--json",
            "--status",
            "PASSED",
        ],
        &[],
    );

    assert_eq!(output.status.code(), Some(0));
    _graphql.assert_calls(1);
}

#[test]
fn without_sorting_the_server_order_is_kept() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue(&server);
    let _auth = mock_auth(&server);
    let _graphql = mock_graphql(
        &server,
        results_page(serde_json::json!([
            run_entry("run-c", "2024-05-01T12:00:00Z".into()),
            serde_json::Value::Null,
            run_entry("run-a", "2024-05-02T10:00:00+02:00".into())
        ])),
    );

    let output = run(&server, &[]);

    assert_eq!(output.status.code(), Some(0));
    _jira_issue.assert_calls(1);
    _auth.assert_calls(1);
    _graphql.assert_calls(1);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        output_ids(&stdout),
        [Some("run-c".to_owned()), None, Some("run-a".to_owned())]
    );
}

#[test]
fn sort_defaults_to_desc_and_compares_instants_across_offsets() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue(&server);
    let _auth = mock_auth(&server);
    // run-a and run-b name the same instant through different offsets; run-c
    // is older, run-d has a garbage timestamp, run-e has none, and the last
    // entry is null.
    let _graphql = mock_graphql(
        &server,
        results_page(serde_json::json!([
            run_entry("run-a", "2024-05-02T10:00:00+02:00".into()),
            run_entry("run-d", "not-a-timestamp".into()),
            run_entry("run-c", "2024-05-01T12:00:00Z".into()),
            run_entry("run-e", serde_json::Value::Null),
            run_entry("run-b", "2024-05-02T09:00:00+01:00".into()),
            serde_json::Value::Null
        ])),
    );

    let output = run_args(
        &server,
        &[
            "xray",
            "test-runs",
            "TEST-123",
            "--json",
            "--sort",
            "started-on",
        ],
        &[],
    );

    assert_eq!(output.status.code(), Some(0));
    _jira_issue.assert_calls(1);
    _auth.assert_calls(1);
    _graphql.assert_calls(1);
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Newest first, ties broken deterministically by run ID, invalid and
    // missing timestamps then null entries last.
    assert_eq!(
        output_ids(&stdout),
        [
            Some("run-a".to_owned()),
            Some("run-b".to_owned()),
            Some("run-c".to_owned()),
            Some("run-d".to_owned()),
            Some("run-e".to_owned()),
            None
        ]
    );
    let value: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["total"], 6);
    assert_eq!(value["returned"], 6);
    assert_eq!(value["truncated"], false);
    assert_eq!(value["start"], 0);
    assert_eq!(value["limit"], 100);
    assert_eq!(value["runs"][3]["startedOn"], "not-a-timestamp");
    assert_eq!(value["runs"][4]["startedOn"], Value::Null);
    assert_eq!(value["runs"][3]["status"], "PASSED");
}

#[test]
fn explicit_asc_sorts_oldest_first_with_the_same_ties_and_tails() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue(&server);
    let _auth = mock_auth(&server);
    let _graphql = mock_graphql(
        &server,
        results_page(serde_json::json!([
            run_entry("run-a", "2024-05-02T10:00:00+02:00".into()),
            run_entry("run-d", "not-a-timestamp".into()),
            run_entry("run-c", "2024-05-01T12:00:00Z".into()),
            run_entry("run-e", serde_json::Value::Null),
            run_entry("run-b", "2024-05-02T09:00:00+01:00".into()),
            serde_json::Value::Null
        ])),
    );

    let output = run_args(
        &server,
        &[
            "xray",
            "test-runs",
            "TEST-123",
            "--json",
            "--sort",
            "started-on",
            "--direction",
            "asc",
        ],
        &[],
    );

    assert_eq!(output.status.code(), Some(0));
    _jira_issue.assert_calls(1);
    _auth.assert_calls(1);
    _graphql.assert_calls(1);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        output_ids(&stdout),
        [
            Some("run-c".to_owned()),
            Some("run-a".to_owned()),
            Some("run-b".to_owned()),
            Some("run-d".to_owned()),
            Some("run-e".to_owned()),
            None
        ]
    );
}

#[test]
fn direction_requires_sort_before_configuration_or_network() {
    let server = MockServer::start();
    let anything = server.mock(|when, then| {
        when.method(GET);
        then.status(200);
    });
    let anything_post = server.mock(|when, then| {
        when.method(POST);
        then.status(200);
    });

    // No environment at all: clap must reject the flag combination before
    // any configuration is read or request is sent.
    let output = jcli(
        &[
            "xray",
            "test-runs",
            "TEST-123",
            "--json",
            "--direction",
            "desc",
        ],
        &[],
    );

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--sort"), "stderr: {stderr}");
    anything.assert_calls(0);
    anything_post.assert_calls(0);
}

#[test]
fn an_unknown_sort_field_is_rejected_by_clap() {
    let server = MockServer::start();
    let anything = server.mock(|when, then| {
        when.method(GET);
        then.status(200);
    });

    let output = jcli(
        &[
            "xray",
            "test-runs",
            "TEST-123",
            "--json",
            "--sort",
            "finished-on",
        ],
        &[
            ("JIRA_BASE_URL", server.base_url().as_str()),
            ("JIRA_EMAIL", "cli@example.com"),
            ("JIRA_API_TOKEN", "cli-test-token"),
            ("XRAY_BASE_URL", server.base_url().as_str()),
            ("XRAY_CLIENT_ID", XRAY_CLIENT_ID),
            ("XRAY_CLIENT_SECRET", XRAY_CLIENT_SECRET),
        ],
    );

    assert_eq!(output.status.code(), Some(2));
    anything.assert_calls(0);
}

/// Fields only the `--details` query selects.
const DETAIL_FIELDS: &[&str] = &[
    "comment",
    "defects",
    "evidence { filename downloadLink }",
    "steps {",
    "actualResult",
    "log",
    "duration",
];

/// Mocks a GraphQL request for TEST-123 (a Test) whose query text must
/// select every detail field.
fn mock_graphql_with_details(server: &MockServer, body: serde_json::Value) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        let mut when = when
            .method(POST)
            .path("/api/v2/graphql")
            .header("Authorization", format!("Bearer {XRAY_TOKEN}"))
            .body_includes("getTestRuns(testIssueIds: $testIssueIds, statuses: $statuses");
        for field in DETAIL_FIELDS {
            when = when.body_includes(*field);
        }
        then.status(200).json_body(body);
    })
}

fn detailed_page() -> serde_json::Value {
    results_page(serde_json::json!([
        {
            "id": "run-1",
            "status": {"name": "FAILED"},
            "startedOn": "2024-05-01T08:00:00+02:00",
            "finishedOn": "2024-05-01T08:01:00+02:00",
            "testExecution": {"issueId": "10500"},
            "comment": "checkout failed on staging",
            "defects": ["10600"],
            "evidence": [{"filename": "report.html", "downloadLink": "https://xray.example/report.html"}],
            "steps": [{
                "status": {"name": "FAILED"},
                "actualResult": "HTTP 500",
                "comment": null,
                "evidence": []
            }],
            "results": [{
                "name": "checkout succeeds",
                "status": {"name": "FAILED"},
                "log": "AssertionError: expected 200, got 500",
                "duration": 1520.5
            }]
        },
        {
            "id": "run-2",
            "status": {"name": "PASSED"},
            "startedOn": null,
            "finishedOn": null,
            "testExecution": null,
            "comment": null,
            "defects": null,
            "evidence": null,
            "steps": [null],
            "results": null
        },
        null
    ]))
}

#[test]
fn details_print_comment_defects_evidence_steps_and_results() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue(&server);
    let _auth = mock_auth(&server);
    let graphql = mock_graphql_with_details(&server, detailed_page());

    let output = run_args(
        &server,
        &["xray", "test-runs", "TEST-123", "--json", "--details"],
        &[],
    );

    graphql.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        value["runs"],
        serde_json::json!([
            {
                "id": "run-1",
                "status": "FAILED",
                "startedOn": "2024-05-01T08:00:00+02:00",
                "finishedOn": "2024-05-01T08:01:00+02:00",
                "testExecutionIssueId": "10500",
                "comment": "checkout failed on staging",
                "defects": ["10600"],
                "evidence": [{"filename": "report.html", "downloadLink": "https://xray.example/report.html"}],
                "steps": [{
                    "status": "FAILED",
                    "actualResult": "HTTP 500",
                    "comment": null,
                    "evidence": []
                }],
                "results": [{
                    "name": "checkout succeeds",
                    "status": "FAILED",
                    "log": "AssertionError: expected 200, got 500",
                    "duration": 1520.5
                }]
            },
            {
                "id": "run-2",
                "status": "PASSED",
                "startedOn": null,
                "finishedOn": null,
                "testExecutionIssueId": null,
                "comment": null,
                "defects": null,
                "evidence": null,
                "steps": [null],
                "results": null
            },
            null
        ])
    );
    assert_eq!(value["returned"], 3);
}

#[test]
fn details_work_for_a_test_execution_key() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue_with_fields(
        &server,
        serde_json::json!({"issuetype": {"name": "Test Execution"}}),
    );
    let _auth = mock_auth(&server);
    let graphql = server.mock(|when, then| {
        let mut when = when
            .method(POST)
            .path("/api/v2/graphql")
            .body_includes("getTestRuns(testExecIssueIds: $testExecIssueIds, statuses: $statuses")
            .body_excludes("testIssueIds");
        for field in DETAIL_FIELDS {
            when = when.body_includes(*field);
        }
        then.status(200).json_body(detailed_page());
    });

    let output = run_args(
        &server,
        &["xray", "test-runs", "TEST-123", "--json", "--details"],
        &[],
    );

    graphql.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    let value: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    assert_eq!(value["issueKind"], "testExecution");
    assert_eq!(
        value["runs"][0]["results"][0]["log"],
        "AssertionError: expected 200, got 500"
    );
}

#[test]
fn without_details_the_request_and_output_have_no_detail_fields() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue(&server);
    let _auth = mock_auth(&server);
    let graphql = server.mock(|when, then| {
        let mut when = when
            .method(POST)
            .path("/api/v2/graphql")
            .body_includes("getTestRuns(testIssueIds: $testIssueIds, statuses: $statuses")
            .body_excludes("steps");
        for field in DETAIL_FIELDS {
            when = when.body_excludes(*field);
        }
        // Xray would not send these fields; jcli must not print them even if it did.
        then.status(200).json_body(detailed_page());
    });

    let output = run(&server, &[]);

    graphql.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    let value: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    assert_eq!(
        value["runs"][0],
        serde_json::json!({
            "id": "run-1",
            "status": "FAILED",
            "startedOn": "2024-05-01T08:00:00+02:00",
            "finishedOn": "2024-05-01T08:01:00+02:00",
            "testExecutionIssueId": "10500"
        })
    );
}
