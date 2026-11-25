//! `tether closeout`: move the sync point over commits that are all decided,
//! without changing a file.

use std::path::Path;

use serde_json::json;

use super::Fork;
use super::sync::{fetch, upstream_commit};
use crate::decisions::{self, Status};
use crate::engine;
use crate::error::{Error, Result};
use crate::report::Report;
use crate::review;
use crate::syncpoint::{self, marker_line};

pub fn run(dir: &Path, rev: &str, execute: bool, no_fetch: bool) -> Result<Report> {
    let fork = Fork::open(dir)?;
    let git = &fork.git;
    if !no_fetch {
        fetch(&fork)?;
    }
    let target = upstream_commit(&fork, rev)?;
    let base = syncpoint::sync_point(git, "HEAD")?.upstream;
    if base == target || !git.is_ancestor(&base, &target)? {
        return Err(Error::Refused(format!(
            "{rev} is not past the sync point {}",
            &base[..12]
        )));
    }
    let range = format!("{base}..{target}");
    let commits: Vec<String> = git
        .raw(&["rev-list", "--reverse", &range])?
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let all = decisions::load(&fork.root)?;
    let undecided: Vec<&String> = commits
        .iter()
        .filter(|sha| all.get(*sha).and_then(|d| d.status).is_none())
        .collect();
    let audit = review::audit(git, &fork.root, &fork.config, &all, &base, &target)?;

    if !undecided.is_empty() || !audit.missing.is_empty() {
        let first = undecided
            .first()
            .map(|sha| format!("tether decide {} merged|ignored --note \"…\"", &sha[..12]))
            .or_else(|| {
                audit.missing.first().map(|m| {
                    format!(
                        "tether decide {} adapted|already-covered|not-applicable --review {} --problem \"…\" --evidence \"…\"",
                        &m.sha[..12],
                        m.area
                    )
                })
            })
            .expect("something is missing");
        return Ok(Report::success(
            "closeout",
            format!(
                "Not ready: {} commit(s) lack a merged/ignored decision, {} review(s) are missing.",
                undecided.len(),
                audit.missing.len()
            ),
            json!({ "target": target, "undecided": undecided, "missing_reviews": audit.missing }),
        )
        .with_next(first, "every commit the sync point passes must be decided")
        .needs_action());
    }

    let counts = json!({
        "target": target,
        "commits": commits.len(),
        "merged": commits.iter().filter(|s| all[*s].status == Some(Status::Merged)).count(),
        "ignored": commits.iter().filter(|s| all[*s].status == Some(Status::Ignored)).count(),
        "reviews": audit.required,
    });
    if !execute {
        let next = format!("tether closeout {} --execute", &target[..12]);
        return Ok(Report::success(
            "closeout",
            format!(
                "Ready: all {} commit(s) through {} are decided.",
                commits.len(),
                &target[..8]
            ),
            counts,
        )
        .with_next(next, "record the close-out"));
    }

    if engine::dirty(git)? || engine::in_merge(git)? || engine::pending_sync(git)?.is_some() {
        return Err(Error::Refused(
            "the working tree must be clean, with no merge or sync in progress".into(),
        ));
    }
    let before = git.ok(&["rev-parse", "HEAD^{tree}"])?;
    let name = fork.config.upstream_name();
    let subject = format!("chore(upstream): close out {name} {}", &target[..8]);
    let body = format!(
        "Every {name} commit through {} is decided; nothing to apply.",
        &target[..12]
    );
    git.ok(&[
        "commit",
        "--allow-empty",
        "-q",
        "-m",
        &subject,
        "-m",
        &body,
        "-m",
        &marker_line(&target)?,
    ])?;
    let after = git.ok(&["rev-parse", "HEAD^{tree}"])?;
    if before != after {
        return Err(Error::Refused(
            "the close-out commit changed the tree; stop and inspect HEAD".into(),
        ));
    }
    Ok(Report::success(
        "closeout",
        format!(
            "Moved the sync point to {}; the tree is unchanged.",
            &target[..8]
        ),
        counts,
    )
    .with_next("tether check", "confirm budgets before pushing"))
}
