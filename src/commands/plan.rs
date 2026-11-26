//! `tether plan`: what to sync, and how far you can go without a conflict.
//!
//! Read-only. It previews merges with `git merge-tree` (the same preview as
//! `sync --dry-run`) at a few checkpoints along upstream's history: the
//! furthest point that merges with no conflict for a person, each release in
//! range, and the tip. It also sorts the pending commits from easiest to
//! hardest by what they touch in the fork.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Serialize;
use serde_json::json;

use super::Fork;
use super::sync;
use crate::error::Result;
use crate::inbox::{self, State};
use crate::report::{Next, Report};

/// How hard one upstream commit is to take, easiest first.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Ease {
    /// Touches nothing the fork changed.
    Clean,
    /// Touches only fork-owned areas, which resolve themselves.
    Owned,
    /// Touches files the fork changed, though the sync merges them cleanly.
    Overlap,
    /// Touches a file the full sync conflicts on.
    Conflict,
}

#[derive(Debug, Serialize)]
struct Step {
    commit: String,
    /// `clean`, `release` or `tip`.
    kind: &'static str,
    /// The release tag, or the commit's subject.
    label: String,
    /// Upstream commits a sync to here brings in.
    commits: usize,
    /// Files a person must resolve.
    conflicts: Vec<String>,
    /// Conflicts in owned areas, which resolve themselves.
    owned: usize,
}

#[derive(Debug, Serialize)]
struct Ranked {
    sha: String,
    ease: Ease,
    /// The fork files behind that rating (conflicting or changed ones).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    files: Vec<String>,
}

pub fn run(dir: &Path) -> Result<Report> {
    let fork = Fork::open(dir)?;
    let inbox = inbox::build(&fork.git, &fork.root, &fork.config)?;
    let base = inbox.sync_point.upstream.clone();
    let target = inbox.target.clone();
    if inbox.behind == 0 {
        let data =
            json!({ "base": base, "target": target, "behind": 0, "steps": [], "commits": [] });
        return Ok(Report::success(
            "plan",
            "Nothing to plan: the fork is at upstream's tip.",
            data,
        ));
    }

    let chain = first_parent(&fork, &base, &target)?;
    let (tip_conflicts, tip_owned) = sync::preview(&fork, &base, &target)?;
    let conflict_paths: HashSet<String> = tip_conflicts
        .iter()
        .map(|path| fork.config.to_upstream_path(path))
        .collect();

    let clean = clean_point(
        &fork,
        &base,
        &chain,
        &conflict_paths,
        tip_conflicts.is_empty(),
    )?;

    // Checkpoints: the clean point, each release in range, the tip.
    let releases: HashMap<String, String> = crate::track::releases(&fork.git, &fork.config)?
        .into_iter()
        .map(|release| (release.commit, release.name))
        .collect();
    let mut points: Vec<(usize, &'static str)> = Vec::new();
    if let Some(index) = clean {
        points.push((index, "clean"));
    }
    for (index, (sha, _)) in chain.iter().enumerate() {
        if releases.contains_key(sha) {
            points.push((index, "release"));
        }
    }
    points.push((chain.len() - 1, "tip"));
    points.sort_by_key(|(index, kind)| (*index, *kind != "clean"));
    points.dedup_by_key(|(index, _)| *index);

    let previews = std::thread::scope(|scope| {
        let handles: Vec<_> = points
            .iter()
            .map(|(index, _)| {
                let (fork, base, chain) = (&fork, &base, &chain);
                let tip = (&tip_conflicts, &tip_owned);
                scope.spawn(move || -> Result<(Vec<String>, Vec<String>, usize)> {
                    let commit = &chain[*index].0;
                    let (person, owned) = if *index == chain.len() - 1 {
                        (tip.0.clone(), tip.1.clone())
                    } else {
                        sync::preview(fork, base, commit)?
                    };
                    let count = fork
                        .git
                        .ok(&["rev-list", "--count", &format!("{base}..{commit}")])?
                        .parse()
                        .unwrap_or(0);
                    Ok((person, owned, count))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("preview thread"))
            .collect::<Result<Vec<_>>>()
    })?;
    let subjects: HashMap<&str, &str> = inbox
        .items
        .iter()
        .map(|item| (item.sha.as_str(), item.subject.as_str()))
        .collect();
    let steps: Vec<Step> = points
        .iter()
        .zip(previews)
        .map(|((index, kind), (conflicts, owned, commits))| {
            let commit = chain[*index].0.clone();
            let label = releases
                .get(&commit)
                .cloned()
                .or_else(|| subjects.get(commit.as_str()).map(|s| (*s).to_owned()))
                .unwrap_or_else(|| commit[..12].to_owned());
            Step {
                commit,
                kind,
                label,
                commits,
                conflicts,
                owned: owned.len(),
            }
        })
        .collect();

    let ranked = rank(&fork, &base, &target, &conflict_paths, &inbox)?;
    let mut counts: HashMap<Ease, usize> = HashMap::new();
    for item in &ranked {
        *counts.entry(item.ease).or_default() += 1;
    }

    let clean_step = steps.iter().find(|step| step.kind == "clean");
    let (summary, next) = match clean_step {
        Some(step) if step.commits >= inbox.behind => (
            format!(
                "All {} upstream commit(s) merge with no conflict for a person.",
                inbox.behind
            ),
            Some(Next::run("tether sync", "nothing in it needs a person")),
        ),
        Some(step) => (
            format!(
                "{} of {} upstream commit(s) merge with no conflict; the full sync conflicts in {} file(s).",
                step.commits,
                inbox.behind,
                tip_conflicts.len()
            ),
            Some(Next::run(
                format!("tether sync --target {}", &step.commit[..12]),
                format!(
                    "brings in {} commit(s) with no conflict; the rest can follow",
                    step.commits
                ),
            )),
        ),
        None => (
            format!(
                "Every step conflicts: even the first upstream commit needs a person; the full sync conflicts in {} file(s).",
                tip_conflicts.len()
            ),
            Some(Next::run(
                "tether sync",
                "there is no conflict-free step; take it all at once",
            )),
        ),
    };
    let data = json!({
        "base": base,
        "target": target,
        "behind": inbox.behind,
        "steps": steps,
        "ease": {
            "clean": counts.get(&Ease::Clean).copied().unwrap_or(0),
            "owned": counts.get(&Ease::Owned).copied().unwrap_or(0),
            "overlap": counts.get(&Ease::Overlap).copied().unwrap_or(0),
            "conflict": counts.get(&Ease::Conflict).copied().unwrap_or(0),
        },
        "commits": ranked,
    });
    let mut report = Report::success("plan", summary, data);
    report.next = next;
    Ok(report)
}

/// Upstream's first-parent history from `base` (exclusive) to `target`,
/// oldest first, with the upstream paths each commit changed.
fn first_parent(fork: &Fork, base: &str, target: &str) -> Result<Vec<(String, Vec<String>)>> {
    let log = fork.git.raw(&[
        "log",
        "--first-parent",
        "--reverse",
        "--diff-merges=first-parent",
        "--name-only",
        "--format=%x00%H",
        &format!("{base}..{target}"),
    ])?;
    Ok(log
        .split('\0')
        .filter(|record| !record.trim().is_empty())
        .map(|record| {
            let mut lines = record.lines().map(str::trim).filter(|l| !l.is_empty());
            let sha = lines.next().unwrap_or_default().to_owned();
            (sha, lines.map(str::to_owned).collect())
        })
        .collect())
}

/// The furthest first-parent commit that merges with no conflict for a
/// person. Conflicts come from files both sides changed, so the step just
/// before the first commit touching a file the full sync conflicts on is the
/// candidate; it is checked, and searched below when it still conflicts.
fn clean_point(
    fork: &Fork,
    base: &str,
    chain: &[(String, Vec<String>)],
    conflict_paths: &HashSet<String>,
    tip_is_clean: bool,
) -> Result<Option<usize>> {
    if tip_is_clean {
        return Ok(Some(chain.len() - 1));
    }
    let first_hit = chain
        .iter()
        .position(|(_, paths)| paths.iter().any(|path| conflict_paths.contains(path)))
        .unwrap_or(chain.len() - 1);
    let is_clean = |index: usize| -> Result<bool> {
        Ok(sync::preview(fork, base, &chain[index].0)?.0.is_empty())
    };
    // Binary search over [0, first_hit): `lo` is known clean (or -1).
    let (mut lo, mut hi) = (-1_isize, first_hit as isize);
    // The candidate first: almost always the answer.
    if hi > 0 {
        if is_clean((hi - 1) as usize)? {
            return Ok(Some((hi - 1) as usize));
        }
        hi -= 1;
    }
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if is_clean(mid as usize)? {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Ok((lo >= 0).then_some(lo as usize))
}

/// Pending commits, easiest first, rated by what they touch in the fork.
fn rank(
    fork: &Fork,
    base: &str,
    target: &str,
    conflict_paths: &HashSet<String>,
    inbox: &inbox::Inbox,
) -> Result<Vec<Ranked>> {
    let upstream = blobs(fork, base)?;
    let ours = blobs(fork, "HEAD")?;
    let log = fork.git.raw(&[
        "log",
        "--name-only",
        "--format=%x00%H",
        &format!("{base}..{target}"),
    ])?;
    let touched: HashMap<String, Vec<String>> = log
        .split('\0')
        .filter(|record| !record.trim().is_empty())
        .map(|record| {
            let mut lines = record.lines().map(str::trim).filter(|l| !l.is_empty());
            let sha = lines.next().unwrap_or_default().to_owned();
            (sha, lines.map(str::to_owned).collect())
        })
        .collect();
    let config = &fork.config;
    let mut ranked: Vec<(i64, Ranked)> = inbox
        .items
        .iter()
        .filter(|item| item.state == State::Pending)
        .map(|item| {
            let rated: Vec<(Ease, String)> = touched
                .get(&item.sha)
                .into_iter()
                .flatten()
                .map(|path| {
                    let home = config.to_fork_path(path);
                    let changed = config.relocations.contains_key(path.as_str())
                        || upstream.get(path.as_str()) != ours.get(home.as_str());
                    let kind = if conflict_paths.contains(path) {
                        Ease::Conflict
                    } else if !changed {
                        Ease::Clean
                    } else if config.is_owned(&home) {
                        Ease::Owned
                    } else {
                        Ease::Overlap
                    };
                    (kind, home)
                })
                .collect();
            let ease = rated
                .iter()
                .map(|(kind, _)| *kind)
                .max()
                .unwrap_or(Ease::Clean);
            let files = rated
                .into_iter()
                .filter(|(kind, _)| *kind == ease && ease != Ease::Clean)
                .map(|(_, home)| home)
                .collect();
            (
                item.timestamp,
                Ranked {
                    sha: item.sha.clone(),
                    ease,
                    files,
                },
            )
        })
        .collect();
    // Easiest first; oldest first within a rating, the order a sync takes them.
    ranked.sort_by_key(|(timestamp, item)| (item.ease, *timestamp));
    Ok(ranked.into_iter().map(|(_, item)| item).collect())
}

/// Every file in `rev`'s tree and its blob id.
fn blobs(fork: &Fork, rev: &str) -> Result<HashMap<String, String>> {
    let out = fork.git.raw(&["ls-tree", "-r", "-z", rev])?;
    Ok(out
        .split('\0')
        .filter_map(|entry| {
            let (meta, path) = entry.split_once('\t')?;
            let blob = meta.split_whitespace().nth(2)?;
            Some((path.to_owned(), blob.to_owned()))
        })
        .collect())
}
