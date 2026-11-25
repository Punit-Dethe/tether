//! Port audit: the check a diff cannot be.
//!
//! When the fork's real code for an upstream file lives elsewhere (the
//! upstream file is inert, logic was relocated, or a fork-only twin shares
//! the bug class), an upstream fix can merge "successfully" and never reach
//! the code that runs. So every synced upstream commit that touched such a
//! file needs a recorded port decision with evidence.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;

use crate::config::Config;
use crate::decisions::{Decisions, PortOutcome, Status};
use crate::error::Result;
use crate::git::Git;
use crate::inbox::Inbox;

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub sha: String,
    pub subject: String,
    /// Relocated upstream files it touched.
    pub files: Vec<String>,
    /// Files with no valid port record yet.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Audit {
    /// Synced, with no record of whether the fix reached the fork's code.
    pub unverified: Vec<Entry>,
    /// Synced, with recorded port decisions.
    pub acknowledged: usize,
    /// Not synced yet; confirm the port when syncing.
    pub pending: Vec<Entry>,
}

/// Full shas queued in the first column of the deferred-ports table, or
/// `None` when no queue is configured (every deferral then counts).
pub fn deferred_queue(root: &Path, config: &Config) -> Result<Option<HashSet<String>>> {
    let Some(file) = &config.port_audit.deferred_queue else {
        return Ok(None);
    };
    let text = match std::fs::read_to_string(root.join(file)) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    let mut queued = HashSet::new();
    for line in text.lines().filter(|l| l.starts_with('|')) {
        let first = line.split('|').nth(1).unwrap_or_default();
        for word in first.split(|c: char| !c.is_ascii_hexdigit()) {
            if word.len() == 40 {
                queued.insert(word.to_ascii_lowercase());
            }
        }
    }
    Ok(Some(queued))
}

pub fn audit(
    git: &Git,
    root: &Path,
    config: &Config,
    decisions: &Decisions,
    inbox: &Inbox,
) -> Result<Audit> {
    let queue = deferred_queue(root, config)?;
    let legacy_through = match &config.port_audit.notes_accepted_through {
        Some(rev) => git.commit_sha(rev)?,
        None => None,
    };
    let mut audit = Audit::default();
    for item in inbox.items.iter().filter(|item| !item.ports.is_empty()) {
        let decision = decisions.get(&item.sha);
        let records = decision.map(|d| &d.port);
        let missing: Vec<String> = item
            .ports
            .iter()
            .filter(|file| {
                records
                    .and_then(|ports| ports.get(*file))
                    .is_none_or(|port| {
                        port.evidence.trim().is_empty()
                            || (port.outcome == PortOutcome::Deferred
                                && queue.as_ref().is_some_and(|q| !q.contains(&item.sha)))
                    })
            })
            .cloned()
            .collect();
        let any_deferred =
            records.is_some_and(|ports| ports.values().any(|p| p.outcome == PortOutcome::Deferred));
        let legacy = match &legacy_through {
            Some(through) => !any_deferred && git.is_ancestor(&item.sha, through)?,
            None => false,
        };
        let noted = decision.is_some_and(|d| {
            d.note.as_deref().is_some_and(|n| !n.trim().is_empty())
                || d.status == Some(Status::Ignored)
        });
        let entry = Entry {
            sha: item.sha.clone(),
            subject: item.subject.clone(),
            files: item.ports.clone(),
            missing: missing.clone(),
        };
        if !item.absorbed {
            audit.pending.push(entry);
        } else if missing.is_empty() || (legacy && noted) {
            audit.acknowledged += 1;
        } else {
            audit.unverified.push(entry);
        }
    }
    Ok(audit)
}
