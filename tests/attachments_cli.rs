use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use httpmock::Method::GET;
use httpmock::MockServer;

const AUTH_HEADER: &str = "Basic Y2xpQGV4YW1wbGUuY29tOmNsaS10ZXN0LXRva2Vu";
const ISSUE_PATH: &str = "/rest/api/3/issue/ABC-123";
const FIELDS: &str = "attachment";

fn run(server: &MockServer, directory: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jcli"))
        .args(args)
        .current_dir(directory)
        .env_clear()
        .env("JIRA_BASE_URL", server.base_url())
        .env("JCLI_ALLOW_INSECURE_HTTP", "1")
        .env("JIRA_EMAIL", "cli@example.com")
        .env("JIRA_API_TOKEN", "cli-test-token")
        .output()
        .expect("jcli should start")
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
        &["issue", "attachments", "ABC-123", "--out", "downloads"],
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
        &["issue", "attachments", "ABC-123", "--out", "chosen"],
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

    let output = run(&server, root.path(), &["issue", "attachments", "ABC-123"]);

    metadata_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"# ABC-123 attachments\n\nNo attachments.\n");
    assert!(output.stderr.is_empty());
    assert!(!root.path().join("ABC-123").exists());
}

#[test]
fn existing_target_file_fails_before_content_and_preserves_bytes() {
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
        &["issue", "attachments", "ABC-123", "--out", "downloads"],
    );

    metadata_mock.assert_calls(1);
    content_mock.assert_calls(0);
    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    assert_eq!(fs::read(&target).unwrap(), b"keep this exact content");
}

#[test]
fn content_failure_removes_nested_directories_and_prints_no_stdout() {
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
    assert!(!output_directory.exists());
    assert!(!root.path().join("nested").exists());
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
        &["issue", "attachments", "ABC-123", "--out", "downloads"],
    );

    metadata_mock.assert_calls(1);
    content_mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(!root.path().join("downloads").exists());
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

    let output = run(&server, root.path(), &["issue", "attachments", "ABC-123"]);

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

        let output = run(&server, root.path(), &["issue", "attachments", "ABC-123"]);

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

        let output = run(&server, root.path(), &["issue", "attachments", "ABC-123"]);

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

    let output = run(&server, root.path(), &["issue", "attachments", "ABC-123"]);

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

    let output = run(&server, root.path(), &["issue", "attachments", "ABC-123"]);

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
fn output_directory_is_private_and_symlink_output_is_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let metadata_mock = metadata(
        &server,
        serde_json::json!([attachment("10001", "file.bin", 4)]),
    );
    let content_mock = content(&server, "/rest/api/3/attachment/content/10001", b"data");

    let output = run(&server, root.path(), &["issue", "attachments", "ABC-123"]);

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
        &["issue", "attachments", "ABC-123", "--out", "linked-output"],
    );

    metadata_mock.assert_calls(1);
    content_mock.assert_calls(0);
    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    assert!(target.read_dir().unwrap().next().is_none());
}
