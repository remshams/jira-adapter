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
    let args: Vec<&str> = vec!["xray", "test-runs", "TEST-123", "--json"];
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
    server.mock(|when, then| {
        when.method(POST)
            .path("/api/v2/graphql")
            .header("Authorization", format!("Bearer {XRAY_TOKEN}"))
            .json_body_includes(
                r#"{"variables": {"testIssueIds": ["10400"], "limit": 100, "start": 0}}"#,
            )
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
