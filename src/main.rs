mod adf;
mod attachments;
mod commands;
mod config;
mod issue_key;
mod jira;
mod xray;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::xray::test_runs::{TestRunSortDirection, TestRunSortField};

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
    /// Work with Xray Cloud test data
    Xray {
        #[command(subcommand)]
        command: XrayCommand,
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

#[derive(Subcommand)]
enum XrayCommand {
    /// List a test issue's test runs as JSON
    TestRuns {
        /// Canonical test issue key (e.g. TEST-123)
        issue: String,
        /// Emit JSON output (required in this version)
        #[arg(long, required = true)]
        json: bool,
        /// Xray run status to keep, e.g. PASSED or a custom status ID.
        /// Repeat for several values; omit to keep every status.
        #[arg(long = "status", value_name = "STATUS")]
        statuses: Vec<String>,
        /// Sort the returned page (not the full history) by this field
        #[arg(long, value_enum, value_name = "FIELD")]
        sort: Option<TestRunSortField>,
        /// Sort order for --sort; defaults to desc
        #[arg(long, value_enum, requires = "sort", value_name = "DIRECTION")]
        direction: Option<TestRunSortDirection>,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Issue { command } => run_issue(command),
        Command::Xray { command } => run_xray(command),
        Command::Whoami => commands::whoami(),
    }
}

fn run_issue(command: IssueCommand) -> ! {
    match command {
        IssueCommand::Read { issue } => commands::read_issue(&issue),
        IssueCommand::Attachments { issue, out } => commands::download_attachments(&issue, out),
    }
}

fn run_xray(command: XrayCommand) -> ! {
    match command {
        XrayCommand::TestRuns {
            issue,
            json: _,
            statuses,
            sort,
            direction,
        } => commands::xray_test_runs(&issue, statuses, sort, direction),
    }
}
