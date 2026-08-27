mod adf;
mod attachments;
mod commands;
mod config;
mod issue_key;
mod jira;

use std::path::PathBuf;

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
            IssueCommand::Read { issue } => commands::read_issue(&issue),
            IssueCommand::Attachments { issue, out } => commands::download_attachments(&issue, out),
        },
        Command::Whoami => commands::whoami(),
    }
}
