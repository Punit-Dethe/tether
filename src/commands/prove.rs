//! `tether prove`: show, from the code, that a sync kept upstream's changes.

use std::path::Path;

use serde_json::json;

use super::Fork;
use crate::engine;
use crate::error::{Error, Result};
use crate::prove::{self, CommitProof, Finding, Verdict};
use crate::report::{Next, NextKind, Report};
use crate::syncpoint;

pub fn run(dir: &Path, from: Option<&str>) -> Result<Report> {
    let fork = Fork::open(dir)?;
    let git = &fork.git;
    let point = syncpoint::sync_point(git, "HEAD")?;

    // A sync in progress is proven as staged; otherwise the last sync, as
    // the fork's code stands now.
    let (to, fork_tree, staged) = match engine::pending_sync(git)? {
        Some(target) => {
            if !engine::conflicted(git)?.is_empty() {
                return Err(Error::Refused(
                    "resolve and `git add` every conflict first; prove checks the merge result"
                        .into(),
                ));
            }
            (target, git.ok(&["write-tree"])?, true)
        }
        None => (point.upstream.clone(), "HEAD^{tree}".to_owned(), false),
    };
    let from = match from {
        Some(rev) => {
            let sha = git
                .commit_sha(rev)?
                .ok_or_else(|| Error::Refused(format!("not a commit: {rev}")))?;
            if !git.is_ancestor(&sha, &to)? {
                return Err(Error::Refused(format!(
                    "{rev} is not in upstream's history before {}",
                    &to[..12]
                )));
            }
            sha
        }
        None if staged => point.upstream.clone(),
        None => previous_sync_point(git, &to)?,
    };

    let decisions = crate::decisions::load(&fork.root)?;
    let commits = prove::prove(git, &fork.config, &decisions, &from, &to, &fork_tree)?;
    let merges: usize = git
        .ok(&["rev-list", "--merges", "--count", &format!("{from}..{to}")])?
        .parse()
        .unwrap_or(0);

    let count = |finding: Finding| commits.iter().filter(|c| c.finding == finding).count();
    let verdicts = |verdict: Verdict| commits.iter().filter(|c| c.verdict == verdict).count();
    let (proven, explained, dropped, contradicted, unproven) = (
        count(Finding::Proven),
        count(Finding::Explained),
        count(Finding::Dropped),
        count(Finding::Contradicted),
        count(Finding::Unproven),
    );
    let span = format!("{}..{}", &from[..8], &to[..8]);
    let what = if staged {
        "the staged sync"
    } else {
        "the fork"
    };
    let merges_note = if merges > 0 {
        format!(
            " {merges} merge {} not checked; their changes are checked through the commits they merged.",
            if merges == 1 {
                "commit is"
            } else {
                "commits are"
            }
        )
    } else {
        String::new()
    };
    let summary = if commits.is_empty() {
        format!("No upstream commits in {span}; nothing to prove.")
    } else if dropped + contradicted + unproven == 0 {
        format!(
            "All {} upstream commits in {span} are accounted for in {what}: {} landed, {} superseded upstream, {} covered by policy, {explained} explained by a decision.{merges_note}",
            commits.len(),
            verdicts(Verdict::Landed),
            verdicts(Verdict::Superseded),
            verdicts(Verdict::Policy),
        )
    } else {
        let mut parts = Vec::new();
        if dropped > 0 {
            parts.push(format!("{dropped} dropped"));
        }
        if contradicted > 0 {
            parts.push(format!("{contradicted} contradict a decision"));
        }
        if unproven > 0 {
            parts.push(format!("{unproven} unproven"));
        }
        format!(
            "{} of {} upstream commits in {span} need attention in {what}; {proven} proven, {explained} explained.{merges_note}",
            parts.join(", "),
            commits.len()
        )
    };

    let data = json!({
        "from": from,
        "to": to,
        "staged": staged,
        "counts": {
            "commits": commits.len(),
            "proven": proven,
            "explained": explained,
            "dropped": dropped,
            "contradicted": contradicted,
            "unproven": unproven,
            "merges_unchecked": merges,
        },
        "commits": commits,
    });
    let mut report = Report::success("prove", summary, data).with_detail(detail(&commits));
    let first = |finding: Finding| commits.iter().find(|c| c.finding == finding);
    if let Some(commit) = first(Finding::Contradicted).or(first(Finding::Dropped)) {
        let short = &commit.sha[..12];
        report = report
            .then(
                Next::of(
                    NextKind::Edit,
                    "tether prove",
                    format!(
                        "upstream's change {short} is not in the fork's code{}: apply it (see the packet), or, if leaving it out is deliberate, record why with `tether decide {short} ignored --note \"<why>\"`",
                        if commit.finding == Finding::Contradicted {
                            " although it is recorded as merged"
                        } else {
                            ""
                        }
                    ),
                )
                .with_packet(format!("tether packet {short}")),
            )
            .needs_action();
    } else if let Some(commit) = first(Finding::Unproven) {
        let short = &commit.sha[..12];
        let next = match commit.review_areas.first() {
            Some(area) if commit.verdict == Verdict::Review => Next::of(
                NextKind::Decide,
                format!(
                    "tether decide {short} adapted|already-covered|not-applicable --review {area} --problem \"<what upstream fixed>\" --evidence \"<what you checked in the fork>\""
                ),
                format!(
                    "upstream changed code in an area the fork owns and the `{area}` review list covers; look at whether the fork has the same problem, port the fix if it does, and record the review"
                ),
            ),
            _ => Next::of(
                NextKind::Decide,
                format!(
                    "tether decide {short} --note \"<where and how the fork carries this change>\""
                ),
                "the fork's code differs where upstream changed it, so tether cannot prove the change is there; check by hand and record what you found",
            ),
        };
        report = report
            .then(next.with_packet(format!("tether packet {short}")))
            .needs_action();
    }
    Ok(report)
}

/// The sync point before `to`: the newest recorded one that `to` contains.
fn previous_sync_point(git: &crate::git::Git, to: &str) -> Result<String> {
    for (_, sha) in syncpoint::markers(git, "HEAD")? {
        if sha != to && git.is_ancestor(&sha, to)? {
            return Ok(sha);
        }
    }
    Err(Error::Refused(
        "only one sync point is recorded, so no sync has happened to prove yet; pass --from <upstream commit> to prove a range"
            .into(),
    ))
}

/// Problems first, each with the files that make it so.
fn detail(commits: &[CommitProof]) -> String {
    let mut out = String::new();
    for (finding, title) in [
        (
            Finding::Contradicted,
            "Recorded as merged, but not in the code",
        ),
        (Finding::Dropped, "Dropped: not in the code, no decision"),
        (
            Finding::Unproven,
            "Unproven: the fork's code differs, or an owned area needs a review",
        ),
        (Finding::Explained, "Explained by a decision"),
    ] {
        let list: Vec<&CommitProof> = commits.iter().filter(|c| c.finding == finding).collect();
        if list.is_empty() {
            continue;
        }
        out.push_str(&format!("{title} ({})\n", list.len()));
        for commit in list {
            out.push_str(&format!("  {} {}\n", &commit.sha[..8], commit.subject));
            if let Some(why) = &commit.explained_by {
                out.push_str(&format!("      {why}\n"));
            }
            for file in commit
                .files
                .iter()
                .filter(|f| f.missing + f.changed + f.review > 0 && finding != Finding::Explained)
            {
                let mut parts = Vec::new();
                if file.review > 0 {
                    parts.push(format!("{} to review", file.review));
                }
                if file.missing > 0 {
                    parts.push(format!("{} missing", file.missing));
                }
                if file.changed > 0 {
                    parts.push(format!("{} changed", file.changed));
                }
                if file.landed > 0 {
                    parts.push(format!("{} landed", file.landed));
                }
                out.push_str(&format!(
                    "      {}: {}{}\n",
                    file.fork_path.as_deref().unwrap_or(&file.path),
                    parts.join(", "),
                    file.why.map(|w| format!(" ({w})")).unwrap_or_default()
                ));
            }
        }
        out.push('\n');
    }
    out
}
