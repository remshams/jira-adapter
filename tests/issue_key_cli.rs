use std::process::{Command, Output};

fn issue_read(value: &str) -> Output {
    jcli(["issue", "read", value])
}

fn issue_attachments(value: &str) -> Output {
    jcli(["issue", "attachments", value])
}

fn jcli<const N: usize>(args: [&str; N]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jcli"))
        .args(args)
        .current_dir(std::env::temp_dir())
        .env("JIRA_BASE_URL", "not a URL")
        .env("JIRA_EMAIL", "unused@example.com")
        .env("JIRA_API_TOKEN", "unused-token")
        .output()
        .expect("jcli should start")
}

#[test]
fn invalid_key_reports_the_parse_error_without_echoing_input() {
    let input = "https://example.atlassian.net/browse/AB-1";
    let output = issue_read(input);
    let stderr = String::from_utf8(output.stderr).unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr,
        "error: invalid Jira issue key: expected PROJECT-NUMBER\n"
    );
    assert!(!stderr.contains(input));
    assert!(output.stdout.is_empty());
}

#[test]
fn valid_key_reaches_configuration_loading() {
    let output = issue_read("ABC-123");

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: invalid URL in JIRA_BASE_URL: expected an HTTPS URL without embedded credentials\n"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn invalid_attachment_key_fails_before_configuration_loading() {
    let input = "https://example.atlassian.net/browse/AB-1";
    let output = issue_attachments(input);
    let stderr = String::from_utf8(output.stderr).unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr,
        "error: invalid Jira issue key: expected PROJECT-NUMBER\n"
    );
    assert!(!stderr.contains(input));
    assert!(output.stdout.is_empty());
}
