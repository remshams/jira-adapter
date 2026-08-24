mod config;
mod issue_key;
mod jira;

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
    // Load .env when present; real environment variables take precedence.
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();

    match cli.command {
        Command::Issue { command } => match command {
            IssueCommand::Read { issue } => {
                let issue = match issue.parse::<issue_key::IssueKey>() {
                    Ok(issue) => issue,
                    Err(err) => exit(1, err),
                };
                eprintln!(
                    "error: 'issue read {}' is not implemented yet",
                    issue.as_str()
                );
                std::process::exit(1);
            }
        },
        Command::Whoami => whoami(),
    }
}

fn whoami() -> ! {
    let config = load_config();
    let client = match jira::client::JiraClient::new(&config) {
        Ok(client) => client,
        Err(err) => exit(3, err),
    };
    match client.myself() {
        Ok(account) => {
            println!(
                "{} <{}> ({})",
                account.display_name.as_deref().unwrap_or("<unknown>"),
                account
                    .email_address
                    .as_deref()
                    .unwrap_or("<no email shown>"),
                account.account_id
            );
            std::process::exit(0);
        }
        Err(err) => exit(3, err),
    }
}

fn load_config() -> config::Config {
    match config::Config::load() {
        Ok(config) => config,
        Err(err) => exit(2, err),
    }
}

fn exit(code: i32, err: impl std::fmt::Display) -> ! {
    eprintln!("error: {err}");
    std::process::exit(code);
}
