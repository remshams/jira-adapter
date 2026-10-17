use std::io::{self, Write};
use std::path::PathBuf;

use crate::adf;
use crate::attachments;
use crate::config;
use crate::issue_key::IssueKey;
use crate::issue_search::IssueSearchOutput;
use crate::jira::client::JiraClient;
use crate::output::terminal_field;
use crate::xray::client::XrayClient;
use crate::xray::test_runs::{TestRunSortDirection, TestRunSortField, TestRunsOutput, sort_page};

pub(crate) fn read_issue(value: &str) -> ! {
    let issue_key = parse_issue_key(value);
    let config = load_config();
    let client = or_exit(JiraClient::new(&config), 3);
    let issue = or_exit(client.issue(&issue_key), 3);
    write_stdout(&adf::render_issue(&issue));
}

pub(crate) fn search_issues(jql: &str, limit: u32) -> ! {
    let config = load_config();
    let client = or_exit(JiraClient::new(&config), 3);
    let page = or_exit(client.search_issues(jql, limit), 3);
    write_stdout(&IssueSearchOutput::from_page(jql, page).to_pretty_json());
}

pub(crate) fn download_attachments(value: &str, output_directory: Option<PathBuf>) -> ! {
    let issue_key = parse_issue_key(value);
    let output_directory = output_directory.unwrap_or_else(|| PathBuf::from(issue_key.as_str()));

    if !cfg!(unix) {
        exit(4, attachments::AttachmentError::UnsupportedPlatform);
    }

    let config = load_config();
    let client = or_exit(JiraClient::new(&config), 3);
    let metadata = or_exit(client.attachments(&issue_key), 3);
    let manifest = match attachments::download_all(&client, &metadata, &output_directory) {
        Ok(manifest) => manifest,
        Err(error) => exit(attachment_exit_code(&error), error),
    };
    write_stdout(&manifest.stdout(&issue_key));
}

fn attachment_exit_code(error: &attachments::AttachmentError) -> i32 {
    if error.is_filesystem() { 4 } else { 3 }
}

pub(crate) fn xray_test_runs(
    value: &str,
    statuses: Vec<String>,
    sort: Option<TestRunSortField>,
    direction: Option<TestRunSortDirection>,
) -> ! {
    let issue_key = parse_issue_key(value);
    // Both configuration sets are loaded before any request so a missing
    // setting fails without touching the network.
    let config = load_config();
    let xray_config = or_exit(config::XrayConfig::load(), 2);
    let jira_client = or_exit(JiraClient::new(&config), 3);
    let issue_id = or_exit(jira_client.issue_id(&issue_key), 3);
    let xray_client = or_exit(XrayClient::new(&xray_config), 3);
    let statuses = if statuses.is_empty() {
        None
    } else {
        Some(statuses.as_slice())
    };
    let mut page = or_exit(xray_client.test_runs(&issue_id, statuses), 3);
    if let Some(field) = sort {
        // A sort without an explicit direction defaults to desc. Sorting
        // applies only to the returned page, never to the full history.
        let direction = direction.unwrap_or(TestRunSortDirection::Desc);
        sort_page(&mut page, field, direction);
    }
    write_stdout(&TestRunsOutput::from_page(issue_key.as_str(), &page).to_pretty_json());
}

pub(crate) fn whoami() -> ! {
    let config = load_config();
    let client = or_exit(JiraClient::new(&config), 3);
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

fn or_exit<T>(result: Result<T, impl std::fmt::Display>, code: i32) -> T {
    match result {
        Ok(value) => value,
        Err(error) => exit(code, error),
    }
}

fn exit(code: i32, error: impl std::fmt::Display) -> ! {
    eprintln!("error: {error}");
    std::process::exit(code);
}
