//! Every way a tether command can fail, each with a stable machine code.
//!
//! Agents branch on [`Error::code`], never on the message text, so codes are
//! part of the public contract: add new ones freely, never rename one.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("git is not installed or not on PATH")]
    GitMissing,

    #[error("git {found} is too old; tether needs git {needed} or newer")]
    GitTooOld { found: String, needed: String },

    #[error("{} is not inside a git repository", .0.display())]
    NotARepo(PathBuf),

    #[error("`git {args}` failed: {stderr}")]
    GitFailed { args: String, stderr: String },

    #[error("`tether {command}` is not built yet (planned for milestone {milestone})")]
    NotImplemented {
        command: &'static str,
        milestone: &'static str,
    },

    #[error("tether is not set up here; run `tether init`")]
    NotConfigured,

    #[error("{path}: {message}")]
    BadConfig { path: String, message: String },

    #[error(
        "no `Upstream-Synced:` line in any commit reachable from {0}; record where \
         this fork last synced with `tether init --sync-point <upstream-commit>`"
    )]
    NoSyncPoint(String),

    #[error("the sync point {0} is not in this clone; fetch the upstream remote")]
    SyncPointMissing(String),

    #[error("sync points diverge: {best} does not contain {stray}")]
    SyncPointsDiverge { best: String, stray: String },

    #[error("{0}")]
    Refused(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// The stable, machine-readable identifier for this failure.
    pub fn code(&self) -> &'static str {
        match self {
            Error::GitMissing => "git_missing",
            Error::GitTooOld { .. } => "git_too_old",
            Error::NotARepo(_) => "not_a_repo",
            Error::GitFailed { .. } => "git_failed",
            Error::NotImplemented { .. } => "not_implemented",
            Error::NotConfigured => "not_configured",
            Error::BadConfig { .. } => "bad_config",
            Error::NoSyncPoint(_) => "no_sync_point",
            Error::SyncPointMissing(_) => "sync_point_missing",
            Error::SyncPointsDiverge { .. } => "sync_points_diverge",
            Error::Refused(_) => "refused",
            Error::Io(_) => "io",
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
