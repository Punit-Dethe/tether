//! What each command does. Every command returns a [`Report`]; none prints.

mod decide;
mod doctor;
mod inbox;
mod init;
mod status;

use std::path::{Path, PathBuf};

use crate::cli::Command;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::git::Git;
use crate::report::Report;

pub fn run(command: &Command, dir: &Path) -> Report {
    let name = command.name();
    let result = match command {
        Command::Doctor => doctor::run(dir),
        Command::Status => status::run(dir),
        Command::Init { sync_point, remote } => init::run(dir, remote, sync_point.as_deref()),
        Command::Inbox { all } => inbox::run(dir, *all),
        Command::Sync => not_built("sync", "M1"),
        Command::Closeout { .. } => not_built("closeout", "M1"),
        Command::Check => not_built("check", "M1"),
        Command::Packet { .. } => not_built("packet", "M2"),
        Command::Decide {
            item,
            outcome,
            note,
            port,
            review,
            problem,
            evidence,
            destinations,
            clear,
        } => decide::run(
            dir,
            decide::Args {
                item,
                outcome: outcome.as_deref(),
                note: note.as_deref(),
                port: port.as_deref(),
                review: review.as_deref(),
                problem: problem.as_deref(),
                evidence: evidence.as_deref(),
                destinations,
                clear: *clear,
            },
        ),
        Command::Verify => not_built("verify", "M3"),
        Command::Map => not_built("map", "M4"),
    };
    result.unwrap_or_else(|error| Report::failure(name, &error))
}

fn not_built(command: &'static str, milestone: &'static str) -> Result<Report> {
    Err(Error::NotImplemented { command, milestone })
}

/// The repository root, git bound to it, and its loaded config.
pub(crate) struct Fork {
    pub root: PathBuf,
    pub git: Git,
    pub config: Config,
}

impl Fork {
    pub fn open(dir: &Path) -> Result<Fork> {
        let root = Git::new(dir).toplevel()?;
        let config = Config::load(&root)?;
        Ok(Fork {
            git: Git::new(&root),
            root,
            config,
        })
    }
}
