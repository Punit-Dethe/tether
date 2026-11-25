//! `tether status`: where things stand, and the one thing to do next.
//!
//! An agent can run a whole sync by following `next` alone. The phases, in
//! the order they are checked:
//!
//! 1. `setup`     — no config, or no sync point recorded yet.
//! 2. `syncing`   — a sync is prepared: resolve conflicts, then continue.
//! 3. `checks`    — `tether check` fails: record ports/reviews, fix budgets.
//! 4. `record`    — decisions are written but not committed.
//! 5. `applied`   — a pending upstream commit was already applied by hand.
//! 6. `behind`    — upstream moved: sync.
//! 7. `clean`     — nothing to do (`next` is null).

use std::path::Path;

use serde_json::json;

use super::Fork;
use super::check::{self, decide_next};
use crate::engine;
use crate::error::{Error, Result};
use crate::git::Git;
use crate::report::{Next, NextKind, Report};
use crate::session;

pub fn run(dir: &Path) -> Result<Report> {
    let fork = match Fork::open(dir) {
        Ok(fork) => fork,
        Err(Error::NotConfigured) => {
            let root = Git::new(dir).toplevel()?;
            return Ok(Report::success(
                "status",
                "tether is not set up in this repository yet.",
                json!({ "phase": "setup", "configured": false, "root": root.display().to_string() }),
            )
            .with_next(
                "tether init",
                "detects the upstream remote and writes .tether/config.toml",
            )
            .needs_action());
        }
        Err(error) => return Err(error),
    };
    let git = &fork.git;

    // 2. A sync in progress outranks everything: it holds the working tree.
    if let Some(target) = engine::pending_sync(git)? {
        let conflicts = engine::conflicted(git)?;
        let session = session::load(git)?;
        let total = session
            .as_ref()
            .map_or(conflicts.len(), |s| s.conflicts.len());
        let data = json!({
            "phase": "syncing",
            "target": target,
            "conflicts": conflicts,
            "resolved": total.saturating_sub(conflicts.len()),
            "total_conflicts": total,
            "session": session,
        });
        let (summary, next) = if conflicts.is_empty() {
            (
                format!("Sync to {} is ready to commit.", &target[..8]),
                Next::run("tether sync --continue", "record the sync commit"),
            )
        } else {
            (
                format!(
                    "Syncing to {}: {} of {total} conflict(s) left.",
                    &target[..8],
                    conflicts.len()
                ),
                super::sync::resolve_next(&conflicts),
            )
        };
        return Ok(Report::success("status", summary, data)
            .then(next)
            .needs_action());
    }

    let eval = match check::evaluate(&fork, None) {
        Ok(eval) => eval,
        Err(Error::NoSyncPoint(_)) => {
            return Ok(Report::success(
                "status",
                "No sync point is recorded yet.",
                json!({ "phase": "setup", "configured": true, "sync_point": null }),
            )
            .then(Next::of(
                NextKind::AskHuman,
                "tether init --sync-point <upstream-commit>",
                "a person must name the upstream commit this fork already contains everything up to",
            ))
            .needs_action());
        }
        Err(error) => return Err(error),
    };
    let inbox = &eval.inbox;
    let pending = inbox.count(crate::inbox::State::Pending);
    let applied: Vec<&str> = inbox
        .items
        .iter()
        .filter(|item| item.applied_here)
        .map(|item| item.sha.as_str())
        .collect();
    let uncommitted = !git
        .ok(&["status", "--porcelain", "--", ".tether"])?
        .is_empty();

    let (phase, summary, next) = if let Some(next) = eval.next {
        (
            "checks",
            format!("{} check(s) need attention.", eval.failures),
            Some(next),
        )
    } else if uncommitted {
        (
            "record",
            "Changes under .tether/ (config, decisions, budgets) are not committed.".to_owned(),
            Some(Next::run(
                "git add .tether && git commit -m \"Record upstream decisions\"",
                "commit the decisions so the next sync and CI see them",
            )),
        )
    } else if let Some(sha) = applied.first() {
        let short = &sha[..12];
        (
            "applied",
            format!("{short} looks applied by hand already."),
            Some(decide_next(
                &fork.config,
                "merged",
                format!("tether decide {short} merged --note \"<where the fork applied it>\""),
                "a fork commit already carries this upstream change; record it so syncs stop replaying it",
                short,
            )),
        )
    } else if inbox.behind > 0 {
        (
            "behind",
            format!(
                "{} upstream commit(s) past the sync point {} ({pending} pending).",
                inbox.behind,
                &inbox.sync_point.upstream[..8]
            ),
            Some(Next::run(
                "tether sync",
                "brings the pending upstream commits in as one sync commit",
            )),
        )
    } else {
        (
            "clean",
            format!(
                "Up to date with {}; all checks pass.",
                fork.config.upstream_ref()
            ),
            None,
        )
    };

    let data = json!({
        "phase": phase,
        "configured": true,
        "sync_point": inbox.sync_point,
        "target": inbox.target,
        "behind": inbox.behind,
        "pending": pending,
        "applied_here": applied,
        "check": eval.data,
    });
    let report = Report::success("status", summary, data);
    Ok(match next {
        Some(next) => report.then(next).needs_action(),
        None => report,
    })
}
