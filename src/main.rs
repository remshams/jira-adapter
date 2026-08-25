mod adf;
mod config;
mod issue_key;
mod jira;

use std::io::{self, Write};

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
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Issue { command } => match command {
            IssueCommand::Read { issue } => {
                let issue = match issue.parse::<issue_key::IssueKey>() {
                    Ok(issue) => issue,
                    Err(err) => exit(1, err),
                };
                read_issue(&issue);
            }
        },
        Command::Whoami => whoami(),
    }
}

fn read_issue(issue_key: &issue_key::IssueKey) -> ! {
    let config = load_config();
    let client = match jira::client::JiraClient::new(&config) {
        Ok(client) => client,
        Err(err) => exit(3, err),
    };
    let issue = match client.issue(issue_key) {
        Ok(issue) => issue,
        Err(err) => exit(3, err),
    };
    let output = adf::render_issue(&issue);
    if let Err(err) = io::stdout().write_all(output.as_bytes()) {
        exit(3, err);
    }
    std::process::exit(0);
}

fn whoami() -> ! {
    let config = load_config();
    let client = match jira::client::JiraClient::new(&config) {
        Ok(client) => client,
        Err(err) => exit(3, err),
    };
    match client.myself() {
        Ok(account) => {
            let display_name = terminal_field(account.display_name.as_deref(), "<unknown>");
            let email = terminal_field(account.email_address.as_deref(), "<no email shown>");
            let account_id = terminal_field(Some(&account.account_id), "<unknown account>");
            println!("{display_name} <{email}> ({account_id})");
            std::process::exit(0);
        }
        Err(err) => exit(3, err),
    }
}

fn load_config() -> config::Config {
    // Load .env only after command arguments that guard configuration access
    // have been validated. Real environment variables still take precedence.
    let _ = dotenvy::dotenv();
    match config::Config::load() {
        Ok(config) => config,
        Err(err) => exit(2, err),
    }
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

fn exit(code: i32, err: impl std::fmt::Display) -> ! {
    eprintln!("error: {err}");
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
