mod common;

use common::jcli;
use httpmock::Method::GET;
use httpmock::MockServer;
use std::process::Output;

// base64 of "cli@example.com:cli-test-token"
const AUTH_HEADER: &str = "Basic Y2xpQGV4YW1wbGUuY29tOmNsaS10ZXN0LXRva2Vu";
const FIELDS: &str =
    "summary,issuetype,status,assignee,reporter,labels,created,description,attachment,issuelinks";

fn run(server: &MockServer) -> Output {
    jcli(
        &["issue", "read", "ABC-123"],
        &[
            ("JIRA_BASE_URL", server.base_url().as_str()),
            ("JIRA_EMAIL", "cli@example.com"),
            ("JIRA_API_TOKEN", "cli-test-token"),
        ],
    )
}

fn issue_mock(server: &MockServer, fields: serde_json::Value) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.method(GET)
            .path("/rest/api/3/issue/ABC-123")
            .query_param("fields", FIELDS)
            .query_param_count(r".*", r".*", 1)
            .header("Authorization", AUTH_HEADER);
        then.status(200)
            .json_body(serde_json::json!({"key": "ABC-123", "fields": fields}));
    })
}

fn remote_links_mock<'a>(server: &'a MockServer, status: u16, body: &str) -> httpmock::Mock<'a> {
    server.mock(|when, then| {
        when.method(GET)
            .path("/rest/api/3/issue/ABC-123/remotelink")
            .query_param_count(r".*", r".*", 0)
            .header("Authorization", AUTH_HEADER);
        then.status(status).body(body);
    })
}

#[test]
fn reads_an_issue_with_exact_request_and_output() {
    let server = MockServer::start();
    let mock = issue_mock(
        &server,
        serde_json::json!({
                "summary": "Fix the thing",
                "issuetype": {"name": "Bug"},
                "status": {"name": "In Progress"},
                "assignee": null,
                "reporter": {
                    "accountId": "private-account-id",
                    "displayName": "Jane Doe",
                    "emailAddress": "jane@example.com"
                },
                "labels": ["backend", "urgent"],
                "created": "2026-10-17T02:13:45.123+0200",
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
        }),
    );
    let remote_mock = remote_links_mock(&server, 200, "[]");

    let output = run(&server);

    mock.assert_calls(1);
    remote_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "# ABC-123: Fix the thing\n\n- Type: Bug\n- Status: In Progress\n- Assignee: Unassigned\n- Reporter: Jane Doe\n- Created: 2026-10-17T02:13:45.123+0200\n- Labels: backend, urgent\n- Attachments: 1\n\n## Description\n\nRendered description.\n\n## Links\n\nNone\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn reads_issue_links_and_remote_links() {
    let server = MockServer::start();
    let mock = issue_mock(
        &server,
        serde_json::json!({
            "summary": "Fix the thing",
            "status": {"name": "Open"},
            "issuelinks": [
                {
                    "id": "10001",
                    "type": {"id": "1", "name": "Blocks", "inward": "is blocked by", "outward": "blocks"},
                    "outwardIssue": {
                        "id": "10002",
                        "key": "ABC-2",
                        "fields": {
                            "summary": "Ship the fix",
                            "status": {"name": "In Progress"},
                            "issuetype": {"name": "Story"}
                        }
                    }
                },
                {
                    "id": "10003",
                    "type": {"id": "1", "name": "Blocks", "inward": "is blocked by", "outward": "blocks"},
                    "inwardIssue": {
                        "id": "10004",
                        "key": "ABC-3",
                        "fields": {"summary": "Find the cause", "status": {"name": "Done"}}
                    }
                }
            ]
        }),
    );
    let remote_mock = remote_links_mock(
        &server,
        200,
        r#"[
            {
                "id": 10000,
                "self": "https://jira.example/rest/api/3/issue/ABC-123/remotelink/10000",
                "application": {"type": "com.github", "name": "GitHub"},
                "relationship": "mentioned in",
                "object": {
                    "url": "https://github.com/acme/repo/pull/7",
                    "title": "acme/repo#7: Fix the thing",
                    "icon": {"url16x16": "https://github.com/favicon.ico"}
                }
            },
            {"id": 10001, "object": {"url": "javascript:alert(1)", "title": "Unsafe"}}
        ]"#,
    );

    let output = run(&server);

    mock.assert_calls(1);
    remote_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "# ABC-123: Fix the thing\n\n- Type: Unknown\n- Status: Open\n- Assignee: Unassigned\n- Reporter: Unknown\n- Created: Unknown\n- Labels: None\n- Attachments: 0\n\n## Description\n\nNo description.\n\n## Links\n\n- blocks ABC-2: Ship the fix (In Progress)\n- is blocked by ABC-3: Find the cause (Done)\n- Remote: acme/repo\\#7: Fix the thing <https://github.com/acme/repo/pull/7>\n- Remote: Unsafe\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn failed_remote_link_request_exits_three_without_stdout() {
    let server = MockServer::start();
    let mock = issue_mock(
        &server,
        serde_json::json!({"summary": "Fix the thing", "status": {"name": "Open"}}),
    );
    let remote_mock = remote_links_mock(&server, 500, "cli-test-token private body");

    let output = run(&server);

    mock.assert_calls(1);
    remote_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: Jira API error (HTTP 500)\n"
    );
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
