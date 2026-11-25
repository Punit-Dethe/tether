//! `tether check`: the gate CI runs on every push.

use std::path::Path;

use serde_json::json;

use super::Fork;
use crate::budget::{self, Baseline};
use crate::decisions;
use crate::error::{Error, Result};
use crate::inbox;
use crate::ports;
use crate::report::Report;
use crate::review;
use crate::syncpoint;

pub struct Args<'a> {
    pub update: bool,
    pub accept_growth: bool,
    pub since: Option<&'a str>,
}

pub fn run(dir: &Path, args: Args) -> Result<Report> {
    let fork = Fork::open(dir)?;
    let git = &fork.git;
    let config = &fork.config;
    let upstream_ref = config.upstream_ref();
    let point = syncpoint::sync_point(git, "HEAD")?;
    let base = &point.upstream;

    let costs = budget::measure(git, config, base)?;
    let strays = budget::strays(git, config, &upstream_ref)?;
    let adopted = budget::adopted(git, config, &upstream_ref)?;
    let baseline = Baseline::load(&fork.root)?;

    let mut growth = Vec::new();
    let mut improvements = Vec::new();
    for (path, cost) in &costs {
        match baseline.files.get(path) {
            None => growth.push(format!(
                "NEW divergence: {path} ({cost} lines), no budget yet"
            )),
            Some(allowed) if cost > allowed => {
                growth.push(format!("GREW: {path} {allowed} -> {cost} lines"));
            }
            Some(allowed) if cost < allowed => {
                improvements.push(format!("shrunk: {path} {allowed} -> {cost}"));
            }
            Some(_) => {}
        }
    }
    for (path, allowed) in &baseline.files {
        if !costs.contains_key(path) {
            improvements.push(format!("converged: {path} {allowed} -> 0"));
        }
    }
    let new_adopted: Vec<&String> = adopted
        .iter()
        .filter(|path| !baseline.adopted.contains(path))
        .collect();
    let released: Vec<&String> = baseline
        .adopted
        .iter()
        .filter(|path| !adopted.contains(path))
        .collect();

    if args.update {
        if !strays.is_empty() {
            return Err(Error::Refused(format!(
                "move the stray upstream file(s) first: {}",
                strays.join(", ")
            )));
        }
        let grew = !growth.is_empty() || !new_adopted.is_empty();
        if grew && !args.accept_growth {
            return Err(Error::Refused(format!(
                "budgets would grow ({}); review it, then rerun with --accept-growth and say why in the commit",
                growth
                    .iter()
                    .map(String::as_str)
                    .chain(new_adopted.iter().map(|p| p.as_str()))
                    .collect::<Vec<_>>()
                    .join("; ")
            )));
        }
        let updated = Baseline {
            files: costs.clone(),
            adopted: adopted.clone(),
        };
        updated.save(&fork.root)?;
        return Ok(Report::success(
            "check",
            format!(
                "Updated .tether/budgets.toml: {} file(s) carry divergence.",
                updated.files.len()
            ),
            json!({ "files": updated.files.len(), "adopted": updated.adopted.len() }),
        )
        .with_next("git add .tether/budgets.toml", "commit the new baseline"));
    }

    let drift = budget::drift(git, config, base, &upstream_ref)?;
    let all = decisions::load(&fork.root)?;
    let inbox = inbox::build(git, &fork.root, config)?;
    let ports = ports::audit(git, config, &all, &inbox)?;

    // Reviews: everything the sync point moved past since `since` (default:
    // the previous sync point).
    let previous = match args.since {
        Some(rev) => Some(syncpoint::sync_point(git, rev)?.upstream),
        None => match git.commit_sha(&format!("{}^", point.commit))? {
            // The recording commit is the root: nothing was synced before it.
            None => None,
            Some(parent) => match syncpoint::sync_point(git, &parent) {
                Ok(previous) => Some(previous.upstream),
                Err(Error::NoSyncPoint(_)) => None,
                Err(error) => return Err(error),
            },
        },
    };
    let reviews = match &previous {
        Some(previous) if previous != base => {
            review::audit(git, &fork.root, config, &all, previous, base)?
        }
        _ => review::Audit::default(),
    };

    let failures = growth.len()
        + strays.len()
        + new_adopted.len()
        + drift.len()
        + ports.unverified.len()
        + reviews.missing.len();
    let data = json!({
        "sync_point": point,
        "budgets": { "files": costs.len(), "failures": growth, "improvements": improvements },
        "strays": strays,
        "adopted": { "new": new_adopted, "released": released, "count": adopted.len() },
        "drift": drift,
        "ports": {
            "unverified": ports.unverified,
            "acknowledged": ports.acknowledged,
            "pending": ports.pending.len(),
        },
        "reviews": { "since": previous, "required": reviews.required, "missing": reviews.missing },
    });
    if failures == 0 {
        let mut report = Report::success(
            "check",
            format!(
                "All checks pass: {} diverged file(s) within budget, {} port record(s), {} review(s).",
                costs.len(),
                ports.acknowledged,
                reviews.required
            ),
            data,
        );
        if !improvements.is_empty() || !released.is_empty() {
            report = report.with_next("tether check --update", "lock in the smaller budgets");
        }
        return Ok(report);
    }
    let next = first_fix(&data);
    Ok(
        Report::success("check", format!("{failures} problem(s) found."), data)
            .with_next(next.0, next.1)
            .needs_action(),
    )
}

/// The single most useful next step for the first failing check.
fn first_fix(data: &serde_json::Value) -> (String, String) {
    if let Some(path) = data["strays"].get(0).and_then(|p| p.as_str()) {
        return (
            format!("git mv {path} <its moved folder>"),
            "a new upstream file landed outside the folder it was moved to".into(),
        );
    }
    if let Some(entry) = data["ports"]["unverified"].get(0) {
        let sha = entry["sha"].as_str().unwrap_or_default();
        let file = entry["missing"][0].as_str().unwrap_or_default();
        return (
            format!(
                "tether decide {} ported|not-applicable|deferred --port {file} --evidence \"…\"",
                &sha[..sha.len().min(12)]
            ),
            "a synced fix to a relocated file has no record of reaching the fork's code".into(),
        );
    }
    if let Some(entry) = data["reviews"]["missing"].get(0) {
        let sha = entry["sha"].as_str().unwrap_or_default();
        let area = entry["area"].as_str().unwrap_or_default();
        return (
            format!(
                "tether decide {} adapted|already-covered|not-applicable --review {area} --problem \"…\" --evidence \"…\"",
                &sha[..sha.len().min(12)]
            ),
            "a synced upstream change in a review area has no recorded review".into(),
        );
    }
    if data["drift"].get(0).is_some() {
        return (
            "tether status".into(),
            "a file matches upstream's latest copy but the sync point is behind it; decide those commits and close out".into(),
        );
    }
    (
        "tether check --update --accept-growth".into(),
        "only after reviewing that the growth is a deliberate hook; say why in the commit".into(),
    )
}
