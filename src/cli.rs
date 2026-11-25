//! Command-line shape. Parsing only; behaviour lives in `commands`.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "tether",
    version,
    about = "Keep a long-lived fork in step with its upstream."
)]
pub struct Cli {
    /// Print the result as JSON (the format agents should use).
    #[arg(long, global = true)]
    pub json: bool,

    /// Run as if started in this directory.
    #[arg(short = 'C', long = "repo", global = true, value_name = "DIR")]
    pub repo: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Detect the upstream, sync point, moved folders and tests; write the config.
    Init,
    /// Where things stand, and the one thing to do next.
    Status,
    /// Upstream changes past the sync point, by category and state.
    Inbox,
    /// A self-contained work packet for one upstream item.
    Packet { item: String },
    /// Run a sync session.
    Sync,
    /// Record a decision, with evidence, for one upstream item.
    Decide { item: String, outcome: String },
    /// Build and test the fork, upstream as shipped, and the result.
    Verify,
    /// The divergence map, generated from the code.
    Map,
    /// The CI gate: budgets, unmarked changes, owned areas, port audit.
    Check,
    /// Move the sync point over fully decided items without changing a file.
    Closeout { rev: String },
    /// Check that git and this repository are usable, in plain words.
    Doctor,
}

impl Command {
    /// The command's name as typed, for reports.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Init => "init",
            Command::Status => "status",
            Command::Inbox => "inbox",
            Command::Packet { .. } => "packet",
            Command::Sync => "sync",
            Command::Decide { .. } => "decide",
            Command::Verify => "verify",
            Command::Map => "map",
            Command::Check => "check",
            Command::Closeout { .. } => "closeout",
            Command::Doctor => "doctor",
        }
    }
}
