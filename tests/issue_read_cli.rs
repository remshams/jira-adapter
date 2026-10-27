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
    run_with(server, &[])
}

fn run_with(server: &MockServer, flags: &[&str]) -> Output {
    let args = [["issue", "read", "ABC-123"].as_slice(), flags].concat();
    jcli(
        &args,
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

fn comments_mock<'a>(
    server: &'a MockServer,
    start_at: &str,
    status: u16,
    body: serde_json::Value,
) -> httpmock::Mock<'a> {
    let start_at = start_at.to_string();
    server.mock(move |when, then| {
        when.method(GET)
            .path("/rest/api/3/issue/ABC-123/comment")
            .query_param("startAt", start_at)
            .query_param("maxResults", "100")
            .query_param("orderBy", "created")
            .query_param_count(r".*", r".*", 3)
            .header("Authorization", AUTH_HEADER);
        then.status(status).json_body(body);
    })
}

fn comment(author: &str, created: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "10000",
        "author": {"accountId": "private-account-id", "displayName": author},
        "created": created,
        "updated": created,
        "body": {
            "version": 1,
            "type": "doc",
            "content": [{"type": "paragraph", "content": [{"type": "text", "text": text}]}]
        }
    })
}

const BARE_ISSUE_PAGE: &str = "# ABC-123: Fix the thing\n\n- Type: Unknown\n- Status: Open\n- Assignee: Unassigned\n- Reporter: Unknown\n- Created: Unknown\n- Labels: None\n- Attachments: 0\n\n## Description\n\nNo description.\n\n## Links\n\nNone\n";

#[test]
fn reads_comments_across_pages_with_the_comments_flag() {
    let server = MockServer::start();
    let mock = issue_mock(
        &server,
        serde_json::json!({"summary": "Fix the thing", "status": {"name": "Open"}}),
    );
    let remote_mock = remote_links_mock(&server, 200, "[]");
    let mut first_page = (1..=99)
        .map(|_| comment("Bot", "2026-10-17T00:00:00.000+0000", "ping"))
        .collect::<Vec<_>>();
    first_page.insert(
        0,
        comment(
            "Jane Doe",
            "2026-10-17T02:13:45.123+0200",
            "Can you reproduce it?",
        ),
    );
    let first = comments_mock(
        &server,
        "0",
        200,
        serde_json::json!({"startAt": 0, "maxResults": 100, "total": 101, "comments": first_page}),
    );
    let second = comments_mock(
        &server,
        "100",
        200,
        serde_json::json!({
            "startAt": 100,
            "maxResults": 100,
            "total": 101,
            "comments": [{
                "author": {"displayName": "John [Roe]"},
                "created": "2026-10-18T09:00:00.000+0200",
                "body": {
                    "version": 1,
                    "type": "doc",
                    "content": [
                        {"type": "heading", "attrs": {"level": 2}, "content": [{"type": "text", "text": "Steps"}]},
                        {"type": "paragraph", "content": [{"type": "text", "text": "Yes."}]}
                    ]
                }
            }]
        }),
    );

    let output = run_with(&server, &["--comments"]);

    mock.assert_calls(1);
    remote_mock.assert_calls(1);
    first.assert_calls(1);
    second.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    let bots = "### Bot · 2026-10-17T00:00:00.000+0000\n\nping\n\n".repeat(99);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "{BARE_ISSUE_PAGE}\n## Comments\n\n### Jane Doe · 2026-10-17T02:13:45.123+0200\n\nCan you reproduce it?\n\n{bots}### John \\[Roe\\] · 2026-10-18T09:00:00.000+0200\n\n##### Steps\n\nYes.\n"
        )
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn an_issue_without_comments_shows_none_with_the_comments_flag() {
    let server = MockServer::start();
    let mock = issue_mock(
        &server,
        serde_json::json!({"summary": "Fix the thing", "status": {"name": "Open"}}),
    );
    let remote_mock = remote_links_mock(&server, 200, "[]");
    let comments = comments_mock(
        &server,
        "0",
        200,
        serde_json::json!({"startAt": 0, "maxResults": 100, "total": 0, "comments": []}),
    );

    let output = run_with(&server, &["--comments"]);

    mock.assert_calls(1);
    remote_mock.assert_calls(1);
    comments.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{BARE_ISSUE_PAGE}\n## Comments\n\nNone\n")
    );
}

#[test]
fn reading_without_the_comments_flag_sends_no_comment_request() {
    let server = MockServer::start();
    let mock = issue_mock(
        &server,
        serde_json::json!({"summary": "Fix the thing", "status": {"name": "Open"}}),
    );
    let remote_mock = remote_links_mock(&server, 200, "[]");
    let comments = server.mock(|when, then| {
        when.method(GET).path("/rest/api/3/issue/ABC-123/comment");
        then.status(200)
            .json_body(serde_json::json!({"total": 1, "comments": [comment("Jane", "2026-10-17T00:00:00Z", "Hidden")]}));
    });

    let output = run(&server);

    mock.assert_calls(1);
    remote_mock.assert_calls(1);
    comments.assert_calls(0);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), BARE_ISSUE_PAGE);
}

#[test]
fn failed_comment_request_exits_three_without_stdout() {
    let server = MockServer::start();
    let mock = issue_mock(
        &server,
        serde_json::json!({"summary": "Fix the thing", "status": {"name": "Open"}}),
    );
    let remote_mock = remote_links_mock(&server, 200, "[]");
    let first = comments_mock(
        &server,
        "0",
        200,
        serde_json::json!({"total": 150, "comments": (1..=100).map(|_| comment("Bot", "2026-10-17T00:00:00Z", "ping")).collect::<Vec<_>>()}),
    );
    let second = comments_mock(
        &server,
        "100",
        500,
        serde_json::json!({"errorMessages": ["cli-test-token private body"]}),
    );

    let output = run_with(&server, &["--comments"]);

    mock.assert_calls(1);
    remote_mock.assert_calls(1);
    first.assert_calls(1);
    second.assert_calls(1);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: Jira API error (HTTP 500)\n"
    );
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
