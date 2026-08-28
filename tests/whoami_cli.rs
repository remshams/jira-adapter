mod common;

use common::jcli;
use httpmock::Method::GET;
use httpmock::MockServer;

// base64 of "cli@example.com:cli-test-token"
const AUTH_HEADER: &str = "Basic Y2xpQGV4YW1wbGUuY29tOmNsaS10ZXN0LXRva2Vu";

#[test]
fn whoami_sanitizes_jira_account_fields() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path("/rest/api/3/myself")
            .header("Authorization", AUTH_HEADER);
        then.status(200).json_body(serde_json::json!({
            "accountId": "account\u{202e}-id",
            "displayName": "Jane\r\nDoe\u{1b}[31m",
            "emailAddress": "jane@example.com\nforged"
        }));
    });

    let output = jcli(
        &["whoami"],
        &[
            ("JIRA_BASE_URL", server.base_url().as_str()),
            ("JIRA_EMAIL", "cli@example.com"),
            ("JIRA_API_TOKEN", "cli-test-token"),
        ],
    );

    mock.assert_calls(1);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Jane Doe[31m <jane@example.com forged> (account-id)\n"
    );
    assert!(output.stderr.is_empty());
}
