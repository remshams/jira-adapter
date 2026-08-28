use std::process::{Command, Output};

use httpmock::Method::GET;
use httpmock::MockServer;

// base64 of "cli@example.com:cli-test-token"
const AUTH_HEADER: &str = "Basic Y2xpQGV4YW1wbGUuY29tOmNsaS10ZXN0LXRva2Vu";
const FIELDS: &str = "summary,status,assignee,reporter,labels,description,attachment";

fn run(server: &MockServer) -> Output {
    // A unique working directory keeps the child process away from any
    // stray .env in the shared temp directory; env_clear already isolates
    // it from HOME and XDG_CONFIG_HOME.
    let dir = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_jcli"))
        .args(["issue", "read", "ABC-123"])
        .current_dir(dir.path())
        .env_clear()
        .env("JIRA_BASE_URL", server.base_url())
        .env("JCLI_ALLOW_INSECURE_HTTP", "1")
        .env("JIRA_EMAIL", "cli@example.com")
        .env("JIRA_API_TOKEN", "cli-test-token")
        .output()
        .expect("jcli should start")
}

#[test]
fn reads_an_issue_with_exact_request_and_output() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path("/rest/api/3/issue/ABC-123")
            .query_param("fields", FIELDS)
            .query_param_count(r".*", r".*", 1)
            .header("Authorization", AUTH_HEADER);
        then.status(200).json_body(serde_json::json!({
            "key": "ABC-123",
            "fields": {
                "summary": "Fix the thing",
                "status": {"name": "In Progress"},
                "assignee": null,
                "reporter": {
                    "accountId": "private-account-id",
                    "displayName": "Jane Doe",
                    "emailAddress": "jane@example.com"
                },
                "labels": ["backend", "urgent"],
                "description": {
                    "version": 1,
                    "type": "doc",
                    "content": [{
                        "type": "paragraph",
                        "content": [{"type": "text", "text": "Rendered description."}]
                    }]
                },
                "attachment": [{
                    "id": "opaque-id",
                    "filename": "report.pdf",
                    "size": 4
                }]
            }
        }));
    });

    let output = run(&server);

    mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "# ABC-123: Fix the thing\n\n- Status: In Progress\n- Assignee: Unassigned\n- Reporter: Jane Doe\n- Labels: backend, urgent\n- Attachments: 1\n\n## Description\n\nRendered description.\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn oversized_issue_metadata_exits_three_without_stdout() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path("/rest/api/3/issue/ABC-123")
            .query_param("fields", FIELDS)
            .query_param_count(r".*", r".*", 1)
            .header("Authorization", AUTH_HEADER);
        then.status(200).body(vec![b' '; 16 * 1024 * 1024 + 1]);
    });

    let output = run(&server);

    mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
}

#[test]
fn jira_error_exits_three_without_stdout() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path("/rest/api/3/issue/ABC-123")
            .query_param("fields", FIELDS)
            .query_param_count(r".*", r".*", 1)
            .header("Authorization", AUTH_HEADER);
        then.status(500)
            .body("Jira\r\nfailed\u{1b}[31m cli@example.com cli-test-token\u{202e}");
    });

    let output = run(&server);

    mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: Jira API error (HTTP 500)\n"
    );
}
