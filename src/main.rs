mod adf;
mod attachment_list;
mod attachment_selection;
mod attachments;
mod commands;
mod config;
mod issue_key;
mod issue_search;
mod jira;
mod output;
mod xray;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use globset::GlobMatcher;

use crate::attachment_selection::AttachmentSelection;

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
    /// List or download an issue's attachments
    Attachments {
        #[command(subcommand)]
        command: AttachmentsCommand,
    },
    /// Search issues with JQL and print the first page as JSON
    Search {
        /// JQL query, passed to Jira unchanged. Use ORDER BY to sort.
        #[arg(long, value_name = "JQL", allow_hyphen_values = true)]
        jql: String,
        /// Emit JSON output (required in this version)
        #[arg(long, required = true)]
        json: bool,
        /// Maximum number of issues to return (1-100)
        #[arg(
            long,
            value_name = "N",
            default_value_t = 20,
            value_parser = clap::value_parser!(u32).range(1..=100)
        )]
        limit: u32,
    },
}

#[derive(Subcommand)]
enum AttachmentsCommand {
    /// List an issue's attachments as JSON without downloading them
    List {
        /// Canonical issue key (e.g. ABC-123)
        issue: String,
        /// Emit JSON output (required in this version)
        #[arg(long, required = true)]
        json: bool,
        #[command(flatten)]
        filters: AttachmentFilters,
    },
    /// Download an issue's attachments, all or a selection (Unix only)
    Download {
        /// Canonical issue key (e.g. ABC-123)
        issue: String,
        /// Destination directory; defaults to ./<issue-key>/
        #[arg(long, value_name = "DIRECTORY")]
        out: Option<PathBuf>,
        #[command(flatten)]
        filters: AttachmentFilters,
    },
}

#[derive(Args)]
struct AttachmentFilters {
    /// Select attachments whose Jira filename matches this case-sensitive
    /// glob. Repeat for several. An attachment matching any --name or --id
    /// is selected; without either, all attachments are selected.
    #[arg(
        long = "name",
        value_name = "GLOB",
        value_parser = attachment_selection::parse_name_pattern
    )]
    names: Vec<GlobMatcher>,
    /// Select the attachment with this ID. Repeat for several.
    #[arg(long = "id", value_name = "ID")]
    ids: Vec<String>,
}

impl AttachmentFilters {
    fn into_selection(self) -> AttachmentSelection {
        AttachmentSelection::new(self.names, self.ids)
    }
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
        IssueCommand::Attachments { command } => run_attachments(command),
        IssueCommand::Search {
            jql,
            json: _,
            limit,
        } => commands::search_issues(&jql, limit),
    }
}

fn run_attachments(command: AttachmentsCommand) -> ! {
    match command {
        AttachmentsCommand::List {
            issue,
            json: _,
            filters,
        } => commands::list_attachments(&issue, &filters.into_selection()),
        AttachmentsCommand::Download {
            issue,
            out,
            filters,
        } => commands::download_attachments(&issue, out, &filters.into_selection()),
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
