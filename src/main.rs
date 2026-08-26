mod adf;
mod attachments;
mod config;
mod issue_key;
mod jira;

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "jcli", version, about = "Read-only Jira Cloud CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Work with Jira issues
    Issue {
        #[command(subcommand)]
        command: IssueCommand,
    },
    /// Print the account the configured credentials belong to
    Whoami,
}

#[derive(Subcommand)]
enum IssueCommand {
    /// Print an issue's content as markdown
    Read {
        /// Canonical issue key (e.g. ABC-123)
        issue: String,
    },
    /// Download all attachments for an issue (Unix only)
    Attachments {
        /// Canonical issue key (e.g. ABC-123)
        issue: String,
        /// Destination directory; defaults to ./<issue-key>/
        #[arg(long, value_name = "DIRECTORY")]
        out: Option<PathBuf>,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Issue { command } => match command {
            IssueCommand::Read { issue } => {
                let issue = parse_issue_key(&issue);
                read_issue(&issue);
            }
            IssueCommand::Attachments { issue, out } => {
                let issue = parse_issue_key(&issue);
                let output_directory = out.unwrap_or_else(|| PathBuf::from(issue.as_str()));
                download_attachments(&issue, &output_directory);
            }
        },
        Command::Whoami => whoami(),
    }
}

fn parse_issue_key(value: &str) -> issue_key::IssueKey {
    match value.parse() {
        Ok(issue) => issue,
        Err(error) => exit(1, error),
    }
}

fn read_issue(issue_key: &issue_key::IssueKey) -> ! {
    let config = load_config();
    let client = match jira::client::JiraClient::new(&config) {
        Ok(client) => client,
        Err(error) => exit(3, error),
    };
    let issue = match client.issue(issue_key) {
        Ok(issue) => issue,
        Err(error) => exit(3, error),
    };
    write_stdout(&adf::render_issue(&issue));
}

fn download_attachments(issue_key: &issue_key::IssueKey, output_directory: &Path) -> ! {
    if !cfg!(unix) {
        exit(4, attachments::AttachmentError::UnsupportedPlatform);
    }

    let config = load_config();
    let client = match jira::client::JiraClient::new(&config) {
        Ok(client) => client,
        Err(error) => exit(3, error),
    };
    let metadata = match client.attachments(issue_key) {
        Ok(metadata) => metadata,
        Err(error) => exit(3, error),
    };
    let manifest = match attachments::download_all(&client, &metadata, output_directory) {
        Ok(manifest) => manifest,
        Err(error) => {
            let code = if error.is_filesystem() { 4 } else { 3 };
            exit(code, error);
        }
    };
    write_stdout(&manifest.stdout(issue_key));
}

fn whoami() -> ! {
    let config = load_config();
    let client = match jira::client::JiraClient::new(&config) {
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

fn load_config() -> config::Config {
    // Load .env only after command arguments that guard configuration access
    // have been validated. Real environment variables still take precedence.
    let _ = dotenvy::dotenv();
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
