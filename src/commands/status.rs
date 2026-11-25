//! `tether status`: where things stand, and the one thing to do next.

use std::path::Path;

use serde_json::json;

use super::Fork;
use crate::error::{Error, Result};
use crate::git::Git;
use crate::inbox::{self, State};
use crate::report::Report;

pub fn run(dir: &Path) -> Result<Report> {
    let fork = match Fork::open(dir) {
        Ok(fork) => fork,
        Err(Error::NotConfigured) => {
            let root = Git::new(dir).toplevel()?;
            return Ok(Report::success(
                "status",
                "tether is not set up in this repository yet.",
                json!({ "configured": false, "root": root.display().to_string() }),
            )
            .with_next(
                "tether init",
                "detects the upstream remote and writes .tether/config.toml",
            )
            .needs_action());
        }
        Err(error) => return Err(error),
    };

    let inbox = match inbox::build(&fork.git, &fork.root, &fork.config) {
        Ok(inbox) => inbox,
        Err(Error::NoSyncPoint(_)) => {
            return Ok(Report::success(
                "status",
                "No sync point is recorded yet.",
                json!({ "configured": true, "sync_point": null }),
            )
            .with_next(
                "tether init --sync-point <upstream-commit>",
                "every comparison is made against the upstream commit the fork last synced to",
            )
            .needs_action());
        }
        Err(error) => return Err(error),
    };

    let pending = inbox.count(State::Pending);
    let applied: Vec<&str> = inbox
        .items
        .iter()
        .filter(|item| item.applied_here)
        .map(|item| item.sha.as_str())
        .collect();
    let data = json!({
        "configured": true,
        "sync_point": inbox.sync_point,
        "target": inbox.target,
        "behind": inbox.behind,
        "pending": pending,
        "applied_here": applied,
    });

    if inbox.behind == 0 {
        return Ok(Report::success(
            "status",
            format!("Up to date with {}.", fork.config.upstream_ref()),
            data,
        ));
    }
    let summary = format!(
        "{} upstream commit(s) past the sync point {} ({pending} pending).",
        inbox.behind,
        &inbox.sync_point.upstream[..8]
    );
    let report = Report::success("status", summary, data);
    let report = if let Some(sha) = applied.first() {
        report.with_next(
            format!("tether decide {} merged --note \"<where the fork applied it>\"", &sha[..12]),
            "a fork commit already carries this upstream change; record it so syncs stop replaying it",
        )
    } else {
        report.with_next(
            "tether sync",
            "brings the pending upstream commits in as one sync commit",
        )
    };
    Ok(report.needs_action())
}
