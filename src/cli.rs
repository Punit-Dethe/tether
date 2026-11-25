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
    /// Detect the upstream and write `.tether/config.toml`; optionally record
    /// the sync point.
    Init {
        /// Upstream commit this fork already contains everything up to.
        /// Records it in an empty `Upstream-Synced:` commit.
        #[arg(long, value_name = "REV")]
        sync_point: Option<String>,
        /// Remote that points at upstream.
        #[arg(long, default_value = "upstream")]
        remote: String,
    },
    /// Where things stand, and the one thing to do next.
    Status,
    /// Tracked upstream commits with their state; pending ones by default.
    Inbox {
        /// Include merged and ignored commits too.
        #[arg(long)]
        all: bool,
    },
    /// A self-contained work packet for one upstream item.
    Packet { item: String },
    /// Run a sync session.
    Sync,
    /// Record a decision about one upstream commit (sha prefix or #PR).
    ///
    ///   decide <item> merged|ignored|pending [--note …]
    ///   decide <item> --note "…"
    ///   decide <item> ported|not-applicable|deferred --port <file> --evidence "…"
    ///   decide <item> adapted|already-covered|not-applicable --review <area>
    ///          --problem "…" --evidence "…" [--dest <path>]…
    ///   decide <item> --clear
    #[command(verbatim_doc_comment)]
    Decide {
        item: String,
        outcome: Option<String>,
        #[arg(long)]
        note: Option<String>,
        /// Relocated upstream file this port record is for.
        #[arg(long, conflicts_with = "review")]
        port: Option<String>,
        /// Review area this review record is for.
        #[arg(long)]
        review: Option<String>,
        /// What upstream fixed (reviews).
        #[arg(long)]
        problem: Option<String>,
        /// How the decision was checked.
        #[arg(long)]
        evidence: Option<String>,
        /// Fork path that carries the change (reviews; repeatable).
        #[arg(long = "dest", value_name = "PATH")]
        destinations: Vec<String>,
        /// Remove every recorded decision for this commit.
        #[arg(long, conflicts_with_all = ["outcome", "note", "port", "review"])]
        clear: bool,
    },
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
            Command::Init { .. } => "init",
            Command::Status => "status",
            Command::Inbox { .. } => "inbox",
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
