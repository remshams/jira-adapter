use std::io::{self, Write};
use std::path::PathBuf;

use crate::adf;
use crate::attachments;
use crate::config;
use crate::issue_key::IssueKey;
use crate::jira::client::JiraClient;
use crate::xray::client::XrayClient;
use crate::xray::test_runs::TestRunsOutput;

pub(crate) fn read_issue(value: &str) -> ! {
    let issue_key = parse_issue_key(value);
    let config = load_config();
    let client = match JiraClient::new(&config) {
        Ok(client) => client,
        Err(error) => exit(3, error),
    };
    let issue = match client.issue(&issue_key) {
        Ok(issue) => issue,
        Err(error) => exit(3, error),
    };
    write_stdout(&adf::render_issue(&issue));
}

pub(crate) fn download_attachments(value: &str, output_directory: Option<PathBuf>) -> ! {
    let issue_key = parse_issue_key(value);
    let output_directory = output_directory.unwrap_or_else(|| PathBuf::from(issue_key.as_str()));

    if !cfg!(unix) {
        exit(4, attachments::AttachmentError::UnsupportedPlatform);
    }

    let config = load_config();
    let client = match JiraClient::new(&config) {
        Ok(client) => client,
        Err(error) => exit(3, error),
    };
    let metadata = match client.attachments(&issue_key) {
        Ok(metadata) => metadata,
        Err(error) => exit(3, error),
    };
    let manifest = match attachments::download_all(&client, &metadata, &output_directory) {
        Ok(manifest) => manifest,
        Err(error) => {
            let code = if error.is_filesystem() { 4 } else { 3 };
            exit(code, error);
        }
    };
    write_stdout(&manifest.stdout(&issue_key));
}

pub(crate) fn xray_test_runs(value: &str) -> ! {
    let issue_key = parse_issue_key(value);
    // Both configuration sets are loaded before any request so a missing
    // setting fails without touching the network.
    let config = load_config();
    let xray_config = match config::XrayConfig::load() {
        Ok(xray) => xray,
        Err(error) => exit(2, error),
    };
    let jira_client = match JiraClient::new(&config) {
        Ok(client) => client,
        Err(error) => exit(3, error),
    };
    let issue_id = match jira_client.issue_id(&issue_key) {
        Ok(issue_id) => issue_id,
        Err(error) => exit(3, error),
    };
    let xray_client = match XrayClient::new(&xray_config) {
        Ok(client) => client,
        Err(error) => exit(3, error),
    };
    let page = match xray_client.test_runs(&issue_id) {
        Ok(page) => page,
        Err(error) => exit(3, error),
    };
    write_stdout(&TestRunsOutput::from_page(issue_key.as_str(), &page).to_pretty_json());
}

pub(crate) fn whoami() -> ! {
    let config = load_config();
    let client = match JiraClient::new(&config) {
        Ok(client) => client,
        Err(error) => exit(3, error),
    };
    match client.myself() {
        Ok(account) => {
            let display_name = terminal_field(account.display_name.as_deref(), "<unknown>");
            let email = terminal_field(account.email_address.as_deref(), "<no email shown>");
            let account_id = terminal_field(Some(&account.account_id), "<unknown account>");
            write_stdout(&format!("{display_name} <{email}> ({account_id})\n"));
        }
        Err(error) => exit(3, error),
    }
}

fn parse_issue_key(value: &str) -> IssueKey {
    match value.parse() {
        Ok(issue) => issue,
        Err(error) => exit(1, error),
    }
}

fn load_config() -> config::Config {
    // Load configuration only after command arguments that guard
    // configuration access have been validated. Config::load parses the
    // exact `.env` in the working directory itself, without mutating the
    // process environment.
    match config::Config::load() {
        Ok(config) => config,
        Err(error) => exit(2, error),
    }
}

fn write_stdout(output: &str) -> ! {
    if let Err(error) = io::stdout().write_all(output.as_bytes()) {
        exit(4, error);
    }
    std::process::exit(0);
}

fn terminal_field(value: Option<&str>, fallback: &str) -> String {
    let Some(value) = value else {
        return fallback.to_string();
    };

    let mut sanitized = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                sanitized.push(' ');
            }
            '\n' | '\t' => sanitized.push(' '),
            character
                if character.is_control()
                    || matches!(
                        character,
                        '\u{061c}'
                            | '\u{200e}'
                            | '\u{200f}'
                            | '\u{202a}'..='\u{202e}'
                            | '\u{2066}'..='\u{2069}'
                    ) => {}
            character => sanitized.push(character),
        }
    }

    if sanitized.trim().is_empty() {
        fallback.to_string()
    } else {
        sanitized
    }
}

fn exit(code: i32, error: impl std::fmt::Display) -> ! {
    eprintln!("error: {error}");
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::terminal_field;

    #[test]
    fn terminal_fields_are_single_line_and_control_free() {
        assert_eq!(
            terminal_field(Some("Jane\r\nDoe\t\u{1b}[31m\u{202e}"), "fallback"),
            "Jane Doe [31m"
        );
        assert_eq!(terminal_field(Some("\u{1b}\u{7}"), "fallback"), "fallback");
        assert_eq!(terminal_field(None, "fallback"), "fallback");
    }
}
