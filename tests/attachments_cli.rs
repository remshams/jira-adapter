mod common;

use common::jcli_in;
use std::fs;
use std::path::Path;
use std::process::Output;

use httpmock::Method::GET;
use httpmock::MockServer;

const AUTH_HEADER: &str = "Basic Y2xpQGV4YW1wbGUuY29tOmNsaS10ZXN0LXRva2Vu";
const ISSUE_PATH: &str = "/rest/api/3/issue/ABC-123";
const FIELDS: &str = "attachment";

fn run(server: &MockServer, directory: &Path, args: &[&str]) -> Output {
    jcli_in(
        directory,
        args,
        &[
            ("JIRA_BASE_URL", server.base_url().as_str()),
            ("JIRA_EMAIL", "cli@example.com"),
            ("JIRA_API_TOKEN", "cli-test-token"),
        ],
    )
}

fn metadata(server: &MockServer, attachments: serde_json::Value) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.method(GET)
            .path(ISSUE_PATH)
            .query_param("fields", FIELDS)
            .query_param_count(r".*", r".*", 1)
            .header("Authorization", AUTH_HEADER);
        then.status(200)
            .json_body(serde_json::json!({"fields": {"attachment": attachments}}));
    })
}

fn attachment(id: &str, filename: &str, size: u64) -> serde_json::Value {
    serde_json::json!({"id": id, "filename": filename, "size": size})
}

fn content<'a>(server: &'a MockServer, id_path: &str, body: &[u8]) -> httpmock::Mock<'a> {
    server.mock(|when, then| {
        when.method(GET)
            .path(id_path)
            .query_param("redirect", "false")
            .query_param_count(r".*", r".*", 1)
            .header("Authorization", AUTH_HEADER);
        then.status(200).body(body);
    })
}

#[test]
fn downloads_binary_files_with_safe_names_and_exact_requests() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let output_directory = root.path().join("downloads");
    let first_id = "doc/opaque?one";
    let second_id = "bin-id";
    let first_name = "id-646f632f6f70617175653f6f6e65-report-draft-.pdf";
    let second_name = "id-62696e2d6964-data.bin";
    let metadata_mock = metadata(
        &server,
        serde_json::json!([
            attachment(first_id, "../report [draft].pdf", 4),
            attachment(second_id, "data.bin", 5),
        ]),
    );
    let first_mock = content(
        &server,
        "/rest/api/3/attachment/content/doc%2Fopaque%3Fone",
        &[0, 255, 10, 128],
    );
    let second_mock = content(
        &server,
        "/rest/api/3/attachment/content/bin-id",
        &[1, 2, 3, 4, 5],
    );

    let output = run(
        &server,
        root.path(),
        &[
            "issue",
            "attachments",
            "download",
            "ABC-123",
            "--out",
            "downloads",
        ],
    );

    metadata_mock.assert_calls(1);
    first_mock.assert_calls(1);
    second_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("# ABC-123 attachments\n\n- {first_name}\n- {second_name}\n")
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        fs::read(output_directory.join(first_name)).unwrap(),
        [0, 255, 10, 128]
    );
    assert_eq!(
        fs::read(output_directory.join(second_name)).unwrap(),
        [1, 2, 3, 4, 5]
    );
}

#[test]
fn out_directory_is_used_directly() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let metadata_mock = metadata(
        &server,
        serde_json::json!([attachment("id", "file.txt", 4)]),
    );
    let content_mock = content(&server, "/rest/api/3/attachment/content/id", b"data");

    let output = run(
        &server,
        root.path(),
        &[
            "issue",
            "attachments",
            "download",
            "ABC-123",
            "--out",
            "chosen",
        ],
    );

    metadata_mock.assert_calls(1);
    content_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert!(root.path().join("chosen").join("id-6964-file.txt").exists());
    assert!(!root.path().join("ABC-123").exists());
}

#[test]
fn no_attachments_prints_markdown_without_creating_default_directory() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let metadata_mock = metadata(&server, serde_json::json!([]));

    let output = run(
        &server,
        root.path(),
        &["issue", "attachments", "download", "ABC-123"],
    );

    metadata_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"# ABC-123 attachments\n\nNo attachments.\n");
    assert!(output.stderr.is_empty());
    assert!(!root.path().join("ABC-123").exists());
}

#[test]
fn existing_target_file_is_replaced_after_a_successful_download() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let output_directory = root.path().join("downloads");
    fs::create_dir(&output_directory).unwrap();
    let target = output_directory.join("id-6964-file.txt");
    fs::write(&target, b"keep this exact content").unwrap();
    let metadata_mock = metadata(
        &server,
        serde_json::json!([attachment("id", "file.txt", 4)]),
    );
    let content_mock = content(&server, "/rest/api/3/attachment/content/id", b"data");

    let output = run(
        &server,
        root.path(),
        &[
            "issue",
            "attachments",
            "download",
            "ABC-123",
            "--out",
            "downloads",
        ],
    );

    metadata_mock.assert_calls(1);
    content_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        output.stdout,
        b"# ABC-123 attachments\n\n- id-6964-file.txt\n"
    );
    assert!(output.stderr.is_empty());
    assert_eq!(fs::read(&target).unwrap(), b"data");
}

#[test]
fn an_unreplaceable_final_directory_is_a_filesystem_failure() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let output_directory = root.path().join("downloads");
    let final_path = output_directory.join("id-6964-file.txt");
    fs::create_dir_all(&final_path).unwrap();
    let metadata_mock = metadata(
        &server,
        serde_json::json!([attachment("id", "file.txt", 4)]),
    );
    let content_mock = content(&server, "/rest/api/3/attachment/content/id", b"data");

    let output = run(
        &server,
        root.path(),
        &[
            "issue",
            "attachments",
            "download",
            "ABC-123",
            "--out",
            "downloads",
        ],
    );

    metadata_mock.assert_calls(1);
    content_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"error: failed to access attachment files\n");
    assert!(final_path.is_dir());
    assert!(!fs::read_dir(&output_directory).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".jcli-attachment-")
    }));
}

#[test]
fn later_content_failure_keeps_an_earlier_download_and_prints_no_stdout() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let output_directory = root.path().join("nested/downloads");
    let metadata_mock = metadata(
        &server,
        serde_json::json!([
            attachment("first", "first.bin", 5),
            attachment("second", "second.bin", 6),
        ]),
    );
    let first_mock = content(&server, "/rest/api/3/attachment/content/first", b"first");
    let second_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/rest/api/3/attachment/content/second")
            .query_param("redirect", "false")
            .query_param_count(r".*", r".*", 1)
            .header("Authorization", AUTH_HEADER);
        then.status(500);
    });

    let output = run(
        &server,
        root.path(),
        &[
            "issue",
            "attachments",
            "download",
            "ABC-123",
            "--out",
            "nested/downloads",
        ],
    );

    metadata_mock.assert_calls(1);
    first_mock.assert_calls(1);
    second_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert_eq!(
        fs::read(output_directory.join("id-6669727374-first.bin")).unwrap(),
        b"first"
    );
    assert!(!fs::read_dir(&output_directory).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".jcli-attachment-")
    }));
}

#[test]
fn size_mismatch_leaves_no_final_files_or_stdout() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let metadata_mock = metadata(
        &server,
        serde_json::json!([attachment("id", "file.bin", 7)]),
    );
    let content_mock = content(&server, "/rest/api/3/attachment/content/id", b"short");

    let output = run(
        &server,
        root.path(),
        &[
            "issue",
            "attachments",
            "download",
            "ABC-123",
            "--out",
            "downloads",
        ],
    );

    metadata_mock.assert_calls(1);
    content_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    let downloads = root.path().join("downloads");
    assert!(downloads.is_dir());
    assert!(fs::read_dir(downloads).unwrap().next().is_none());
}

#[test]
fn unsafe_filename_cannot_escape_or_add_terminal_or_markdown_structure() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let metadata_mock = metadata(
        &server,
        serde_json::json!([attachment(
            "unsafe/id",
            "../../evil\n# forged\u{1b}[31m.txt",
            4
        )]),
    );
    let content_mock = content(
        &server,
        "/rest/api/3/attachment/content/unsafe%2Fid",
        b"safe",
    );

    let output = run(
        &server,
        root.path(),
        &["issue", "attachments", "download", "ABC-123"],
    );

    metadata_mock.assert_calls(1);
    content_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        output.stdout,
        b"# ABC-123 attachments\n\n- id-756e736166652f6964-evil-forged-31m.txt\n"
    );
    assert!(output.stderr.is_empty());
    assert!(
        root.path()
            .join("ABC-123")
            .join("id-756e736166652f6964-evil-forged-31m.txt")
            .is_file()
    );
    assert!(!root.path().join("evil").exists());
}

#[test]
fn dot_ids_fail_before_content_requests() {
    for id in [".", ".."] {
        let server = MockServer::start();
        let root = tempfile::tempdir().unwrap();
        let metadata_mock = metadata(&server, serde_json::json!([attachment(id, "file.bin", 0)]));
        let content_mock = server.mock(|when, then| {
            when.method(GET).path_includes("/attachment/content/");
            then.status(200);
        });

        let output = run(
            &server,
            root.path(),
            &["issue", "attachments", "download", "ABC-123"],
        );

        metadata_mock.assert_calls(1);
        content_mock.assert_calls(0);
        assert_eq!(output.status.code(), Some(3));
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains(id));
    }
}

#[test]
fn attachment_limits_fail_before_content_requests() {
    let cases = [
        (0..=1_000)
            .map(|index| attachment(&index.to_string(), "file.bin", 0))
            .collect::<Vec<_>>(),
        vec![attachment("1", "file.bin", 20 * 1024 * 1024 * 1024 + 1)],
        (0..6)
            .map(|index| attachment(&index.to_string(), "file.bin", 20 * 1024 * 1024 * 1024))
            .collect::<Vec<_>>(),
    ];

    for attachments in cases {
        let server = MockServer::start();
        let root = tempfile::tempdir().unwrap();
        let metadata_mock = metadata(&server, serde_json::Value::Array(attachments));
        let content_mock = server.mock(|when, then| {
            when.method(GET).path_includes("/attachment/content/");
            then.status(200);
        });

        let output = run(
            &server,
            root.path(),
            &["issue", "attachments", "download", "ABC-123"],
        );

        metadata_mock.assert_calls(1);
        content_mock.assert_calls(0);
        assert_eq!(output.status.code(), Some(3));
        assert!(output.stdout.is_empty());
        assert!(!root.path().join("ABC-123").exists());
    }
}

#[test]
fn oversized_metadata_response_fails_without_truncating_or_downloading() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let metadata_mock = server.mock(|when, then| {
        when.method(GET)
            .path(ISSUE_PATH)
            .query_param("fields", FIELDS);
        then.status(200).body(vec![b' '; 16 * 1024 * 1024 + 1]);
    });
    let content_mock = server.mock(|when, then| {
        when.method(GET).path_includes("/attachment/content/");
        then.status(200);
    });

    let output = run(
        &server,
        root.path(),
        &["issue", "attachments", "download", "ABC-123"],
    );

    metadata_mock.assert_calls(1);
    content_mock.assert_calls(0);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
}

#[test]
fn zero_byte_download_keeps_exact_stdout_contract() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let metadata_mock = metadata(
        &server,
        serde_json::json!([attachment("10001", "empty.bin", 0)]),
    );
    let content_mock = content(&server, "/rest/api/3/attachment/content/10001", b"");

    let output = run(
        &server,
        root.path(),
        &["issue", "attachments", "download", "ABC-123"],
    );

    metadata_mock.assert_calls(1);
    content_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        output.stdout,
        b"# ABC-123 attachments\n\n- 10001-empty.bin\n"
    );
    assert_eq!(
        fs::metadata(root.path().join("ABC-123/10001-empty.bin"))
            .unwrap()
            .len(),
        0
    );
}

#[cfg(unix)]
#[test]
fn output_directory_is_private_and_a_user_provided_symlink_is_used() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let metadata_mock = metadata(
        &server,
        serde_json::json!([attachment("10001", "file.bin", 4)]),
    );
    let content_mock = content(&server, "/rest/api/3/attachment/content/10001", b"data");

    let output = run(
        &server,
        root.path(),
        &["issue", "attachments", "download", "ABC-123"],
    );

    metadata_mock.assert_calls(1);
    content_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        fs::metadata(root.path().join("ABC-123"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    let second_server = MockServer::start();
    let target = root.path().join("target");
    fs::create_dir(&target).unwrap();
    let link = root.path().join("linked-output");
    symlink(&target, &link).unwrap();
    let metadata_mock = metadata(
        &second_server,
        serde_json::json!([attachment("10002", "file.bin", 4)]),
    );
    let content_mock = content(
        &second_server,
        "/rest/api/3/attachment/content/10002",
        b"data",
    );

    let output = run(
        &second_server,
        root.path(),
        &[
            "issue",
            "attachments",
            "download",
            "ABC-123",
            "--out",
            "linked-output",
        ],
    );

    metadata_mock.assert_calls(1);
    content_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        output.stdout,
        b"# ABC-123 attachments\n\n- 10002-file.bin\n"
    );
    assert!(output.stderr.is_empty());
    assert_eq!(fs::read(target.join("10002-file.bin")).unwrap(), b"data");
}

#[test]
fn list_prints_metadata_as_json_without_downloading() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let metadata_mock = metadata(
        &server,
        serde_json::json!([
            {
                "id": "10002",
                "filename": "nightly-report.html",
                "size": 2048,
                "created": "2026-10-26T02:13:45.123+0100",
                "mimeType": "text/html",
                "content": "https://untrusted.example/content"
            },
            attachment("10001", "log.txt", 7)
        ]),
    );
    let content_mock = server.mock(|when, then| {
        when.method(GET).path_includes("/attachment/content/");
        then.status(200);
    });

    let output = run(
        &server,
        root.path(),
        &["issue", "attachments", "list", "ABC-123", "--json"],
    );

    metadata_mock.assert_calls(1);
    content_mock.assert_calls(0);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        r#"{
  "issue": "ABC-123",
  "returned": 2,
  "attachments": [
    {
      "id": "10002",
      "filename": "nightly-report.html",
      "size": 2048,
      "created": "2026-10-26T02:13:45.123+0100",
      "mimeType": "text/html"
    },
    {
      "id": "10001",
      "filename": "log.txt",
      "size": 7,
      "created": null,
      "mimeType": null
    }
  ]
}
"#
    );
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn list_without_attachments_prints_an_empty_list() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let metadata_mock = metadata(&server, serde_json::json!([]));

    let output = run(
        &server,
        root.path(),
        &["issue", "attachments", "list", "ABC-123", "--json"],
    );

    metadata_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        output.stdout,
        b"{\n  \"issue\": \"ABC-123\",\n  \"returned\": 0,\n  \"attachments\": []\n}\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn list_reports_a_jira_failure_with_exit_3_and_empty_stdout() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let metadata_mock = server.mock(|when, then| {
        when.method(GET).path(ISSUE_PATH);
        then.status(404);
    });

    let output = run(
        &server,
        root.path(),
        &["issue", "attachments", "list", "ABC-123", "--json"],
    );

    metadata_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("error: "));
    assert_eq!(stderr.lines().count(), 1);
}

#[test]
fn list_requires_json_and_the_bare_form_is_a_usage_error() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let metadata_mock = server.mock(|when, then| {
        when.any_request();
        then.status(200);
    });

    for args in [
        &["issue", "attachments", "list", "ABC-123"][..],
        &["issue", "attachments", "ABC-123"][..],
    ] {
        let output = run(&server, root.path(), args);

        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
    }
    metadata_mock.assert_calls(0);
}

fn no_content_requests(server: &MockServer) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.method(GET).path_includes("/attachment/content/");
        then.status(200);
    })
}

fn report_ticket(server: &MockServer) -> httpmock::Mock<'_> {
    metadata(
        server,
        serde_json::json!([
            attachment("10001", "nightly-report.html", 6),
            attachment("10002", "server.log", 3),
            attachment("10003", "Summary.HTML", 7),
            attachment("10004", "screenshot.png", 3),
        ]),
    )
}

#[test]
fn filtered_download_writes_and_requests_only_selected_attachments() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let metadata_mock = report_ticket(&server);
    let report_mock = content(&server, "/rest/api/3/attachment/content/10001", b"report");
    let screenshot_mock = content(&server, "/rest/api/3/attachment/content/10004", b"png");
    let other_mock = server.mock(|when, then| {
        when.method(GET)
            .path_matches(r"/attachment/content/1000[23]$");
        then.status(200);
    });

    let output = run(
        &server,
        root.path(),
        &[
            "issue",
            "attachments",
            "download",
            "ABC-123",
            "--name",
            "*.html",
            "--id",
            "10004",
        ],
    );

    metadata_mock.assert_calls(1);
    report_mock.assert_calls(1);
    screenshot_mock.assert_calls(1);
    other_mock.assert_calls(0);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        output.stdout,
        b"# ABC-123 attachments\n\n- 10001-nightly-report.html\n- 10004-screenshot.png\n"
    );
    assert!(output.stderr.is_empty());
    let mut files: Vec<String> = fs::read_dir(root.path().join("ABC-123"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    assert_eq!(files, ["10001-nightly-report.html", "10004-screenshot.png"]);
}

#[test]
fn name_filter_matches_the_jira_filename_not_the_name_on_disk() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let metadata_mock = metadata(
        &server,
        serde_json::json!([attachment("10001", "report [draft].pdf", 4)]),
    );
    let content_mock = content(&server, "/rest/api/3/attachment/content/10001", b"data");

    let output = run(
        &server,
        root.path(),
        &[
            "issue",
            "attachments",
            "download",
            "ABC-123",
            "--name",
            "report ?draft?.pdf",
        ],
    );

    metadata_mock.assert_calls(1);
    content_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        output.stdout,
        b"# ABC-123 attachments\n\n- 10001-report-draft-.pdf\n"
    );
}

#[test]
fn limits_apply_to_the_selected_attachments_only() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let metadata_mock = metadata(
        &server,
        serde_json::json!([
            attachment("10001", "huge.bin", 20 * 1024 * 1024 * 1024 + 1),
            attachment("10002", "small.txt", 4),
        ]),
    );
    let content_mock = content(&server, "/rest/api/3/attachment/content/10002", b"data");

    let output = run(
        &server,
        root.path(),
        &[
            "issue",
            "attachments",
            "download",
            "ABC-123",
            "--id",
            "10002",
        ],
    );

    metadata_mock.assert_calls(1);
    content_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        output.stdout,
        b"# ABC-123 attachments\n\n- 10002-small.txt\n"
    );
}

#[test]
fn download_filter_matching_nothing_prints_no_attachments_and_creates_no_directory() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let metadata_mock = report_ticket(&server);
    let content_mock = no_content_requests(&server);

    let output = run(
        &server,
        root.path(),
        &[
            "issue",
            "attachments",
            "download",
            "ABC-123",
            "--name",
            "summary.html",
        ],
    );

    metadata_mock.assert_calls(1);
    content_mock.assert_calls(0);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"# ABC-123 attachments\n\nNo attachments.\n");
    assert!(output.stderr.is_empty());
    assert!(!root.path().join("ABC-123").exists());
}

#[test]
fn filtered_list_prints_only_selected_attachments() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let metadata_mock = report_ticket(&server);
    let content_mock = no_content_requests(&server);

    let output = run(
        &server,
        root.path(),
        &[
            "issue",
            "attachments",
            "list",
            "ABC-123",
            "--json",
            "--name",
            "*.HTML",
            "--name",
            "*.log",
        ],
    );

    metadata_mock.assert_calls(1);
    content_mock.assert_calls(0);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["returned"], 2);
    assert_eq!(json["attachments"][0]["id"], "10002");
    assert_eq!(json["attachments"][1]["id"], "10003");
}

#[test]
fn list_filter_matching_nothing_prints_an_empty_list() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let metadata_mock = report_ticket(&server);

    let output = run(
        &server,
        root.path(),
        &[
            "issue",
            "attachments",
            "list",
            "ABC-123",
            "--json",
            "--id",
            "99999",
        ],
    );

    metadata_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        output.stdout,
        b"{\n  \"issue\": \"ABC-123\",\n  \"returned\": 0,\n  \"attachments\": []\n}\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn an_invalid_glob_is_a_usage_error_before_any_request() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let any_mock = server.mock(|when, then| {
        when.any_request();
        then.status(200);
    });

    for args in [
        &[
            "issue",
            "attachments",
            "list",
            "ABC-123",
            "--json",
            "--name",
            "report[",
        ][..],
        &[
            "issue",
            "attachments",
            "download",
            "ABC-123",
            "--name",
            "report[",
        ][..],
    ] {
        let output = run(&server, root.path(), args);

        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.starts_with("error: invalid value 'report[' for '--name <GLOB>'"),
            "{stderr}"
        );
    }
    any_mock.assert_calls(0);
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn an_invalid_glob_is_reported_before_missing_configuration() {
    let root = tempfile::tempdir().unwrap();

    let output = jcli_in(
        root.path(),
        &["issue", "attachments", "download", "ABC-123", "--name", "["],
        &[],
    );

    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .starts_with("error: invalid value '[' for '--name <GLOB>'")
    );
}
