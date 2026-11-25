//! `tether verify`: build and test the fork before the sync, upstream as
//! shipped, and the result; only failures new in the result block.

use std::path::Path;

use serde_json::{Value, json};

use super::Fork;
use crate::engine;
use crate::error::{Error, Result};
use crate::report::{Next, NextKind, Report};
use crate::session;
use crate::syncpoint;
use crate::verify::{self, Run, Target, Verdict};

pub struct Args {
    pub baseline: bool,
    pub upstream: bool,
    pub result: bool,
    pub force: bool,
}

/// What the three targets are right now.
struct Targets {
    /// The fork before the sync (none when nothing was synced before).
    baseline: Option<String>,
    upstream: String,
    /// A sync is prepared and waiting to be committed.
    syncing: bool,
}

fn targets(fork: &Fork) -> Result<Targets> {
    let git = &fork.git;
    if let Some(target) = engine::pending_sync(git)? {
        let conflicts = engine::conflicted(git)?;
        if !conflicts.is_empty() {
            return Err(Error::Refused(format!(
                "resolve the sync's conflicts first: {}",
                conflicts.join(", ")
            )));
        }
        return Ok(Targets {
            baseline: git.commit_sha("HEAD")?,
            upstream: target,
            syncing: true,
        });
    }
    let point = syncpoint::sync_point(git, "HEAD")?;
    Ok(Targets {
        baseline: git.commit_sha(&format!("{}^", point.commit))?,
        upstream: point.upstream,
        syncing: false,
    })
}

/// Run one target, or reuse its last run when the tree has not changed.
fn run_target(
    fork: &Fork,
    target: Target,
    commit: Option<&str>,
    force: bool,
) -> Result<(Run, bool)> {
    let git = &fork.git;
    let key = match commit {
        Some(commit) => verify::commit_key(git, commit)?,
        None => verify::working_key(git)?,
    };
    if !force {
        if let Some(run) = verify::load_run(git, target)? {
            if run.key == key {
                return Ok((run, true));
            }
        }
    }
    let settings = &fork.config.verify;
    let steps = match commit {
        Some(commit) => {
            verify::require_commit(git, commit)?;
            verify::in_worktree(git, target, commit, |dir| {
                verify::execute(git, settings, target, dir)
            })?
        }
        None => verify::execute(git, settings, target, &fork.root)?,
    };
    let run = Run {
        target,
        commit: commit.map(str::to_owned),
        key,
        at: session::now(),
        steps,
    };
    verify::save_run(git, &run)?;
    Ok((run, false))
}

fn describe(run: &Run, cached: bool) -> Value {
    json!({
        "target": run.target,
        "commit": run.commit,
        "cached": cached,
        "failures": run.failures(),
        "steps": run.steps,
    })
}

pub fn run(dir: &Path, args: Args) -> Result<Report> {
    let fork = Fork::open(dir)?;
    if fork.config.verify.is_empty() {
        return Err(Error::Refused(
            "no build or test steps are configured; add [[verify.step]] entries (name, run, format) to .tether/config.toml".into(),
        ));
    }
    let targets = targets(&fork)?;
    let single = [
        (args.baseline, Target::Baseline),
        (args.upstream, Target::Upstream),
        (args.result, Target::Result),
    ]
    .into_iter()
    .find_map(|(chosen, target)| chosen.then_some(target));

    if let Some(target) = single {
        let commit = match target {
            Target::Baseline => Some(targets.baseline.clone().ok_or_else(|| {
                Error::Refused("there is no fork commit before the sync point to test".into())
            })?),
            Target::Upstream => Some(targets.upstream.clone()),
            Target::Result => None,
        };
        let (run, cached) = run_target(&fork, target, commit.as_deref(), args.force)?;
        let failures = run.failures();
        let summary = format!(
            "{}: {} failure(s){}.",
            target.label(),
            failures.len(),
            if cached {
                " (unchanged tree, last run reused)"
            } else {
                ""
            }
        );
        return Ok(Report::success("verify", summary, describe(&run, cached)));
    }

    // The result first: when it passes, nothing else needs building.
    let (result, result_cached) = run_target(&fork, Target::Result, None, args.force)?;
    let mut runs = vec![describe(&result, result_cached)];
    let (baseline, upstream) = if result.failures().is_empty() {
        (None, None)
    } else {
        let baseline = match &targets.baseline {
            Some(commit) => Some(run_target(
                &fork,
                Target::Baseline,
                Some(commit),
                args.force,
            )?),
            None => None,
        };
        let upstream = run_target(&fork, Target::Upstream, Some(&targets.upstream), args.force)?;
        if let Some((run, cached)) = &baseline {
            runs.push(describe(run, *cached));
        }
        runs.push(describe(&upstream.0, upstream.1));
        (baseline.map(|b| b.0), Some(upstream.0))
    };
    let verdict = verify::compare(&result, baseline.as_ref(), upstream.as_ref());
    verify::save_verdict(&fork.git, &verdict)?;

    let summary = if verdict.new.is_empty() {
        format!(
            "No new failures ({} inherited from upstream, {} already in the fork, {} fixed).",
            verdict.inherited.len(),
            verdict.preexisting.len(),
            verdict.fixed.len()
        )
    } else {
        format!(
            "The result has {} failure(s) neither the fork nor upstream had.",
            verdict.new.len()
        )
    };
    let data = json!({ "verdict": verdict, "runs": runs, "syncing": targets.syncing });
    let report = Report::success("verify", summary, data);
    Ok(if !verdict.new.is_empty() {
        report.then(fix_next(&verdict)).needs_action()
    } else if targets.syncing {
        report
            .with_next(
                "tether sync --continue",
                "the result builds and tests no worse than its inputs",
            )
            .needs_action()
    } else {
        report
    })
}

fn fix_next(verdict: &Verdict) -> Next {
    Next::of(
        NextKind::Edit,
        "tether verify",
        format!(
            "fix what the merge broke ({}); `tether verify --result` shows each step's log, then rerun",
            verdict.new.join(", ")
        ),
    )
}

/// What must happen before a prepared sync may be committed, if anything:
/// run verify, or fix what it found. `None` when verify is not configured
/// or the current tree passed.
pub(crate) fn gate(fork: &Fork) -> Result<Option<Next>> {
    if fork.config.verify.is_empty() {
        return Ok(None);
    }
    let key = verify::working_key(&fork.git)?;
    Ok(match verify::load_verdict(&fork.git)? {
        Some(verdict) if verdict.key == key && verdict.new.is_empty() => None,
        Some(verdict) if verdict.key == key => Some(fix_next(&verdict)),
        _ => Some(Next::run(
            "tether verify",
            "build and test the result (and, if it fails, the fork and upstream) before committing the sync",
        )),
    })
}
