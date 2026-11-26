//! `tether status`: where things stand, and the one thing to do next.
//!
//! An agent can run a whole sync by following `next` alone. The phases, in
//! the order they are checked:
//!
//! 1. `setup`     — no config, or no sync point recorded yet.
//! 2. `syncing`   — a sync is prepared: resolve conflicts, verify, continue.
//! 3. `checks`    — `tether check` fails: record ports/reviews, fix budgets.
//! 4. `record`    — decisions are written but not committed.
//! 5. `applied`   — a pending upstream commit was already applied by hand.
//! 6. `behind`    — upstream moved: sync (security fixes named first).
//! 7. `security`  — tracking releases, and upstream fixed a security issue
//!    that no release ships yet: a person decides.
//! 8. `clean`     — nothing to do (`next` is null).

use std::path::Path;

use serde_json::json;

use super::Fork;
use super::check::{self, decide_next};
use crate::engine;
use crate::error::{Error, Result};
use crate::git::Git;
use crate::inbox::{Item, State};
use crate::report::{Next, NextKind, Report};
use crate::session;

/// The selected upstream's status; for the main one, the extra upstreams'
/// too, so one `tether status` covers the whole repository.
pub fn run(dir: &Path) -> Result<Report> {
    let mut report = run_one(dir)?;
    if crate::config::selected_upstream().is_some() {
        return Ok(report);
    }
    let root = Git::new(dir).toplevel()?;
    let extras = crate::config::extra_upstreams(&root);
    if extras.is_empty() {
        return Ok(report);
    }
    let mut upstreams = Vec::new();
    let mut lines = Vec::new();
    for name in &extras {
        crate::config::select_upstream(Some(name))?;
        let theirs = run_one(dir);
        crate::config::select_upstream(None)?;
        let theirs = theirs.unwrap_or_else(|error| Report::failure("status", &error));
        let next = theirs.next.clone().map(|mut next| {
            if let Some(rest) = next.command.strip_prefix("tether ") {
                next.command = format!("tether -u {name} {rest}");
            }
            next
        });
        lines.push(format!("{name}: {}", theirs.summary));
        // The main upstream's next step comes first; an extra one's only
        // when the main one has nothing to do.
        if report.next.is_none() {
            if let Some(next) = next.clone() {
                report = report.then(next).needs_action();
            }
        }
        upstreams.push(json!({
            "name": name,
            "ok": theirs.ok,
            "summary": theirs.summary,
            "phase": theirs.data.get("phase"),
            "next": next,
        }));
    }
    if let Some(data) = report.data.as_object_mut() {
        data.insert("upstreams".into(), json!(upstreams));
    }
    report.detail = Some(format!("Other upstreams\n  {}", lines.join("\n  ")));
    Ok(report)
}

fn run_one(dir: &Path) -> Result<Report> {
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
            match super::verify::gate(&fork)? {
                Some(next) => (
                    format!(
                        "Sync to {}: conflicts resolved; verify the result.",
                        &target[..8]
                    ),
                    next,
                ),
                None => (
                    format!("Sync to {} is ready to commit.", &target[..8]),
                    Next::run("tether sync --continue", "record the sync commit"),
                ),
            }
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
    let pending = inbox.count(State::Pending);
    let applied: Vec<&str> = inbox
        .items
        .iter()
        .filter(|item| item.applied_here)
        .map(|item| item.sha.as_str())
        .collect();
    let mut tracked_state = vec![".tether"];
    if let Some(shared) = &fork.config.sync.rerere_cache {
        tracked_state.push(shared);
    }
    let mut porcelain = vec!["status", "--porcelain", "--"];
    porcelain.extend(tracked_state.iter().copied());
    let uncommitted = !git.ok(&porcelain)?.is_empty();
    let record_command = format!(
        "git add {} && git commit -m \"Record upstream decisions\"",
        tracked_state.join(" ")
    );
    let security = |state: State| -> Vec<&crate::inbox::Item> {
        inbox
            .items
            .iter()
            .filter(|i| i.state == state && !i.security.is_empty())
            .collect()
    };
    let pending_security = security(State::Pending);
    let unreleased_security = security(State::Unreleased);

    let (phase, summary, next) = if let Some(next) = eval.next {
        (
            "checks",
            format!("{} check(s) need attention.", eval.failures),
            Some(next),
        )
    } else if uncommitted {
        (
            "record",
            format!(
                "Changes under {}/ (config, decisions, budgets) are not committed.",
                crate::config::state_dir()
            ),
            Some(Next::run(
                record_command,
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
        let mut summary = format!(
            "{} upstream commit(s) past the sync point {} ({pending} pending).",
            inbox.behind,
            &inbox.sync_point.upstream[..8]
        );
        if !pending_security.is_empty() {
            summary = format!(
                "{} pending security fix(es): {}. {summary}",
                pending_security.len(),
                names(&pending_security)
            );
        }
        (
            "behind",
            summary,
            Some(Next::run(
                "tether sync",
                "brings the pending upstream commits in as one sync commit",
            )),
        )
    } else if let Some(first) = unreleased_security.first() {
        (
            "security",
            format!(
                "Upstream fixed {} security issue(s) no release ships yet: {}.",
                unreleased_security.len(),
                names(&unreleased_security)
            ),
            Some(Next::of(
                NextKind::AskHuman,
                format!("tether sync --target {}", &first.sha[..12]),
                "the fork tracks releases; syncing past the newest one to take an unreleased security fix is a person's call",
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
        "release": inbox.release,
        "unreleased": inbox.unreleased,
        "security": {
            "pending": pending_security.iter().map(|i| &i.sha).collect::<Vec<_>>(),
            "unreleased": unreleased_security.iter().map(|i| &i.sha).collect::<Vec<_>>(),
        },
        "check": eval.data,
    });
    let report = Report::success("status", summary, data);
    Ok(match next {
        Some(next) => report.then(next).needs_action(),
        None => report,
    })
}

/// `abc123def456 (GHSA-…)` for each item, its first signal in brackets.
fn names(items: &[&Item]) -> String {
    items
        .iter()
        .map(|i| format!("{} ({})", &i.sha[..12], i.security[0]))
        .collect::<Vec<_>>()
        .join(", ")
}
