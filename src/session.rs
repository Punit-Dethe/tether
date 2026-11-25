//! The sync in progress, kept in `.git/tether/session.json` (never committed).
//!
//! git's own state says *that* a sync is in progress (the prepared
//! `SQUASH_MSG` carries its marker). The session adds what an agent needs to
//! resume after a crash or a context reset: where the sync started, every
//! conflict it raised and what the owned-area rules already settled.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::git::Git;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Session {
    /// The sync point the merge was made against.
    pub base: String,
    /// The upstream commit being synced to.
    pub target: String,
    /// Seconds since the Unix epoch.
    pub started: u64,
    /// Every conflict the merge raised that needed a person.
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub owned_resolved: Vec<String>,
    #[serde(default)]
    pub owned_reverted: Vec<String>,
}

fn path(git: &Git) -> Result<std::path::PathBuf> {
    git.git_path("tether/session.json")
}

pub fn load(git: &Git) -> Result<Option<Session>> {
    match std::fs::read_to_string(path(git)?) {
        Ok(text) => Ok(serde_json::from_str(&text).ok()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn save(git: &Git, session: &Session) -> Result<()> {
    let path = path(git)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(session).expect("a session is always serialisable");
    std::fs::write(path, text)?;
    Ok(())
}

pub fn clear(git: &Git) -> Result<()> {
    match std::fs::remove_file(path(git)?) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
