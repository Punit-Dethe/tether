//! `tether sync`: bring upstream in as one squashed, single-parent commit.

use std::path::Path;

use serde_json::json;

use super::Fork;
use crate::autonomy;
use crate::engine;
use crate::error::{Error, Result};
use crate::report::{Next, NextKind, Report};
use crate::session::{self, Session};
use crate::syncpoint;
use crate::track;

pub struct Args<'a> {
    pub target: Option<&'a str>,
    pub dry_run: bool,
    pub continue_: bool,
    pub abort: bool,
    pub no_fetch: bool,
    pub skip_verify: bool,
}

pub fn run(dir: &Path, args: Args) -> Result<Report> {
    let fork = Fork::open(dir)?;
    if args.continue_ {
        return finish(&fork, args.skip_verify);
    }
    if args.abort {
        return abort(&fork);
    }
    start(&fork, &args)
}

/// Fetch the upstream remote so the target is what upstream has now.
pub(crate) fn fetch(fork: &Fork) -> Result<()> {
    track::fetch(&fork.git, &fork.config)
}

/// Resolve `rev` to an upstream commit on the tracked branch.
pub(crate) fn upstream_commit(fork: &Fork, rev: &str) -> Result<String> {
    let sha = fork
        .git
        .commit_sha(rev)?
        .ok_or_else(|| Error::Refused(format!("not a commit: {rev}")))?;
    if !track::in_upstream(&fork.git, &fork.config, &sha)? {
        return Err(Error::Refused(format!(
            "{rev} is not upstream's: not on {} or in a tracked release",
            fork.config.upstream_ref()
        )));
    }
    Ok(sha)
}

fn start(fork: &Fork, args: &Args) -> Result<Report> {
    let git = &fork.git;
    if !args.no_fetch {
        fetch(fork)?;
    }
    let target = match args.target {
        Some(rev) => upstream_commit(fork, rev)?,
        None => track::target(git, &fork.config)?.commit,
    };
    if fork.config.upstream.track == crate::config::Track::Releases
        && !track::releases(git, &fork.config)?
            .iter()
            .any(|r| git.is_ancestor(&target, &r.commit).unwrap_or(false))
    {
        // Past every release: only with a person's say-so.
        autonomy::require(&fork.config, "sync-unreleased")?;
    }
    let base = syncpoint::sync_point(git, "HEAD")?.upstream;
    if !git.is_ancestor(&base, &target)? {
        return Err(Error::Refused(format!(
            "{} does not contain the sync point {}",
            &target[..12],
            &base[..12]
        )));
    }
    let range = format!("{base}..{target}");
    let behind: usize = git
        .ok(&["rev-list", "--count", &range])?
        .parse()
        .unwrap_or(0);
    if behind == 0 {
        return Ok(Report::success(
            "sync",
            format!("Already synced through {}.", &target[..8]),
            json!({ "base": base, "target": target, "behind": 0 }),
        ));
    }

    if args.dry_run {
        let preview = preview(fork, &base, &target)?;
        let summary = format!(
            "{behind} upstream commit(s) to bring in; {} conflict(s) need a person, {} resolve themselves (owned areas).",
            preview.0.len(),
            preview.1.len()
        );
        return Ok(Report::success(
            "sync",
            summary,
            json!({
                "dry_run": true, "base": base, "target": target, "behind": behind,
                "conflicts": preview.0, "owned_conflicts": preview.1,
            }),
        )
        .with_next("tether sync", "start the sync"));
    }

    if engine::in_merge(git)? || engine::pending_sync(git)?.is_some() {
        return Err(Error::Refused(
            "a merge or sync is already in progress; finish it with `tether sync --continue` or drop it with `tether sync --abort`".into(),
        ));
    }
    if engine::dirty(git)? {
        return Err(Error::Refused(
            "the working tree has uncommitted changes; commit or stash them first".into(),
        ));
    }

    let restored = match &fork.config.sync.rerere_cache {
        Some(shared) => engine::copy_resolutions(
            &fork.root.join(shared),
            &git.git_path("rr-cache")?,
            engine::CacheDirection::Restore,
        )?,
        None => 0,
    };
    let merged = engine::squash_merge(git, &fork.config.sync.merge_settings(), &base, &target)?;
    let conflicts = engine::conflicted(git)?;
    if merged.status > 1 || (merged.status == 1 && conflicts.is_empty()) {
        return Err(Error::GitFailed {
            args: "merge --squash".into(),
            stderr: format!("{}{}", merged.stdout, merged.stderr)
                .trim()
                .to_owned(),
        });
    }
    engine::write_squash_msg(
        git,
        &engine::sync_message(git, &fork.config, &base, &target)?,
    )?;
    let owned = engine::apply_owned(git, &fork.config, &target)?;
    let remaining = engine::conflicted(git)?;
    session::save(
        git,
        &Session {
            base: base.clone(),
            target: target.clone(),
            started: session::now(),
            conflicts: remaining.clone(),
            owned_resolved: owned.resolved.clone(),
            owned_reverted: owned.reverted.clone(),
        },
    )?;

    let data = json!({
        "base": base, "target": target, "behind": behind,
        "conflicts": remaining,
        "owned_resolved": owned.resolved,
        "owned_reverted": owned.reverted,
        "state_reverted": owned.state_reverted,
        "rerere_restored": restored,
    });
    if !owned.state_reverted.is_empty() {
        // Rename detection paired an upstream file with tether's own state:
        // its upstream change was dropped, and only a person can place it.
        return Ok(Report::success(
            "sync",
            format!(
                "Upstream edits were routed into tether's own files ({}) and were NOT applied.",
                owned.state_reverted.join(", ")
            ),
            data,
        )
        .then(Next::of(
            NextKind::Edit,
            "tether sync --continue",
            "find the upstream file each reverted state file was paired with (`git diff -M` from the base), apply its upstream change to the fork's copy by hand, then continue",
        ))
        .needs_action());
    }
    if remaining.is_empty() {
        let next = super::verify::gate(fork)?
            .unwrap_or_else(|| Next::run("tether sync --continue", "record the sync commit"));
        return Ok(Report::success(
            "sync",
            format!("{behind} upstream commit(s) merged with no conflicts left."),
            data,
        )
        .then(next)
        .needs_action());
    }
    Ok(Report::success(
        "sync",
        format!("{} conflict(s) need a decision.", remaining.len()),
        data,
    )
    .then(resolve_next(&remaining))
    .needs_action())
}

/// Resolve the listed conflicts, then continue.
pub(crate) fn resolve_next(conflicts: &[String]) -> Next {
    let first = &conflicts[0];
    Next::of(
        NextKind::Edit,
        "tether sync --continue",
        format!(
            "resolve {} conflicted file(s) ({}), keeping the fork's intent and upstream's fix, remove every conflict marker and `git add` each",
            conflicts.len(),
            conflicts.join(", ")
        ),
    )
    .with_packet(format!("tether packet {first}"))
}

/// Conflicts a merge would raise, split into (needs a person, owned area),
/// computed in memory without touching the working tree.
fn preview(fork: &Fork, base: &str, target: &str) -> Result<(Vec<String>, Vec<String>)> {
    let merge_base = format!("--merge-base={base}");
    // The real merge's settings ([sync] git_config: merge drivers, rename
    // handling), or the preview reports conflicts the sync would not raise.
    let settings = fork.config.sync.merge_settings();
    let mut args: Vec<&str> = Vec::new();
    for setting in &settings {
        args.extend(["-c", setting.as_str()]);
    }
    args.extend([
        "merge-tree",
        "--write-tree",
        "--name-only",
        &merge_base,
        "HEAD",
        target,
    ]);
    let out = fork.git.run(&args)?;
    if out.status > 1 {
        return Err(Error::GitFailed {
            args: "merge-tree".into(),
            stderr: out.stderr.trim().to_owned(),
        });
    }
    let mut person = Vec::new();
    let mut owned = Vec::new();
    for path in out
        .stdout
        .lines()
        .skip(1)
        .take_while(|line| !line.is_empty())
    {
        let relocated_away = fork
            .config
            .relocations
            .contains_key(&fork.config.to_upstream_path(path))
            && !fork
                .git
                .run(&["cat-file", "-e", &format!("HEAD:{path}")])?
                .success();
        let is_owned = fork.config.is_owned(path) || relocated_away;
        if is_owned {
            owned.push(path.to_owned());
        } else {
            person.push(path.to_owned());
        }
    }
    Ok((person, owned))
}

fn finish(fork: &Fork, skip_verify: bool) -> Result<Report> {
    let git = &fork.git;
    let Some(target) = engine::pending_sync(git)? else {
        return Err(Error::Refused("no sync is in progress".into()));
    };
    let remaining = engine::conflicted(git)?;
    if !remaining.is_empty() {
        return Ok(Report::success(
            "sync",
            format!("{} conflict(s) are still unresolved.", remaining.len()),
            json!({ "conflicts": remaining, "target": target }),
        )
        .then(resolve_next(&remaining))
        .needs_action());
    }
    let markers = files_with_markers(fork)?;
    if !markers.is_empty() {
        return Err(Error::Refused(format!(
            "conflict markers are still in {}; finish resolving before committing",
            markers.join(", ")
        )));
    }
    if skip_verify {
        autonomy::require(&fork.config, "skip-verify")?;
    } else if let Some(next) = super::verify::gate(fork)? {
        return Ok(Report::success(
            "sync",
            "The result must pass `tether verify` before the sync is committed.",
            json!({ "target": target }),
        )
        .then(next)
        .needs_action());
    }
    let commit = engine::commit_sync(git, &fork.config.sync.merge_settings())?;
    session::clear(git)?;
    // Share what was resolved, so no clone resolves it again.
    let saved = match &fork.config.sync.rerere_cache {
        Some(shared) => engine::copy_resolutions(
            &git.git_path("rr-cache")?,
            &fork.root.join(shared),
            engine::CacheDirection::Save,
        )?,
        None => 0,
    };
    let report = Report::success(
        "sync",
        format!("Recorded the sync to {}.", &target[..8]),
        json!({ "commit": commit, "target": target, "rerere_saved": saved }),
    );
    Ok(match (&fork.config.sync.rerere_cache, saved) {
        (Some(shared), 1..) => report.with_next(
            format!("git add {shared} && git commit -m \"Share new conflict resolutions\""),
            "commit the new rerere resolutions so other clones and CI reuse them",
        ),
        _ => report.with_next("tether check", "confirm budgets and reviews before pushing"),
    })
}

fn abort(fork: &Fork) -> Result<Report> {
    let git = &fork.git;
    if engine::pending_sync(git)?.is_none() {
        return Err(Error::Refused(
            "no tether sync is in progress; nothing was changed".into(),
        ));
    }
    autonomy::require(&fork.config, "abort")?;
    git.ok(&["reset", "--hard", "--quiet", "HEAD"])?;
    engine::remove_squash_msg(git)?;
    session::clear(git)?;
    Ok(Report::success(
        "sync",
        "Dropped the prepared sync; the working tree is back at HEAD.",
        json!({}),
    ))
}

/// Files the sync touched that still hold conflict markers (a `git add`
/// on an unresolved file clears the conflict flag but not the markers).
fn files_with_markers(fork: &Fork) -> Result<Vec<String>> {
    let staged = fork.git.raw(&["diff", "--cached", "--name-only", "HEAD"])?;
    let mut found = Vec::new();
    for path in staged.lines().map(str::trim).filter(|p| !p.is_empty()) {
        let Ok(text) = std::fs::read(fork.root.join(path)) else {
            continue;
        };
        let has = |marker: &[u8]| {
            text.split(|&b| b == b'\n')
                .any(|line| line.starts_with(marker))
        };
        if has(b"<<<<<<< ") && has(b">>>>>>> ") {
            found.push(path.to_owned());
        }
    }
    Ok(found)
}
