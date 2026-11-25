//! Port audit: the check a diff cannot be.
//!
//! When the fork's real code for an upstream file lives elsewhere (the
//! upstream file is inert, logic was relocated, or a fork-only twin shares
//! the bug class), an upstream fix can merge "successfully" and never reach
//! the code that runs. So every synced upstream commit that touched such a
//! file needs a recorded port decision with evidence.

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

pub fn audit(git: &Git, config: &Config, decisions: &Decisions, inbox: &Inbox) -> Result<Audit> {
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
                    .is_none_or(|port| port.evidence.trim().is_empty())
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
