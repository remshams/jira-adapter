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
    server.mock(|when, then| {
        when.method(GET)
            .path("/rest/api/3/issue/TEST-123")
            .query_param("fields", "summary")
            .query_param_count(r".*", r".*", 1)
            .header("Authorization", JIRA_AUTH_HEADER);
        then.status(200).json_body(serde_json::json!({
            "id": "10400",
            "key": "TEST-123",
            "fields": {"summary": "Check login"}
        }));
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
            .body_includes("getTestRuns");
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
    assert_eq!(value["testKey"], "TEST-123");
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
fn truncation_compares_total_with_the_number_returned() {
    let server = MockServer::start();
    let _jira_issue = mock_jira_issue(&server);
    let _auth = mock_auth(&server);
    let _graphql = mock_graphql(&server, page_body(5, 3));

    let output = run(&server, &[]);

    assert_eq!(output.status.code(), Some(0));
    let value: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    assert_eq!(value["total"], 5);
    assert_eq!(value["returned"], 3);
    assert_eq!(value["truncated"], true);
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
    assert_eq!(value["testKey"], "TEST-123");
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
