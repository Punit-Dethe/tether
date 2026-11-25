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
    /// Bring upstream in as one squashed sync commit.
    ///
    ///   sync [--target <rev>] [--dry-run]   start (owned areas resolve themselves)
    ///   sync --continue                     commit once conflicts are resolved
    ///   sync --abort                        drop the prepared sync
    #[command(verbatim_doc_comment)]
    Sync {
        /// Upstream commit to sync to (default: the upstream branch tip).
        #[arg(long, value_name = "REV")]
        target: Option<String>,
        /// Report what would happen, including conflicts, and change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Commit the prepared sync.
        #[arg(long = "continue", conflicts_with_all = ["target", "dry_run", "abort"])]
        continue_: bool,
        /// Throw away the prepared sync and its working-tree changes.
        #[arg(long, conflicts_with_all = ["target", "dry_run"])]
        abort: bool,
        /// Use the already-fetched upstream branch.
        #[arg(long)]
        no_fetch: bool,
    },
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
    /// The CI gate: budgets, strays, owned areas, drift, port audit, reviews.
    Check {
        /// Record the current numbers as the new baseline (shrinking only).
        #[arg(long)]
        update: bool,
        /// With --update: also accept growth, after reviewing it.
        #[arg(long, requires = "update")]
        accept_growth: bool,
        /// Check reviews for everything synced since this fork revision
        /// (default: since the previous sync point).
        #[arg(long, value_name = "REV")]
        since: Option<String>,
    },
    /// Move the sync point over fully decided commits without changing a file.
    Closeout {
        /// Upstream commit to move the sync point to.
        rev: String,
        /// Record it (default: only check).
        #[arg(long)]
        execute: bool,
        /// Use the already-fetched upstream branch.
        #[arg(long)]
        no_fetch: bool,
    },
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
            Command::Sync { .. } => "sync",
            Command::Decide { .. } => "decide",
            Command::Verify => "verify",
            Command::Map => "map",
            Command::Check { .. } => "check",
            Command::Closeout { .. } => "closeout",
            Command::Doctor => "doctor",
        }
    }
}
