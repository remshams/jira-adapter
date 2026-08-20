mod config;

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
}

#[derive(Subcommand)]
enum IssueCommand {
    /// Print an issue's content as markdown
    Read {
        /// Issue key (e.g. ABC-123) or browse URL
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
                // Validate the environment early so mistyped commands fail
                // with a config error instead of later, deeper in the flow.
                if let Err(err) = config::Config::load() {
                    eprintln!("error: {err}");
                    std::process::exit(2);
                }
                eprintln!("error: 'issue read {issue}' is not implemented yet");
                std::process::exit(1);
            }
        },
    }
}
