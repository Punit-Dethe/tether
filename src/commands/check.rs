//! `tether check`: the gate CI runs on every push.

use std::path::Path;

use serde_json::{Value, json};

use super::Fork;
use crate::autonomy;
use crate::budget::{self, Baseline};
use crate::decisions;
use crate::error::{Error, Result};
use crate::inbox::{self, Inbox};
use crate::map;
use crate::ports;
use crate::report::{Next, NextKind, Report};
use crate::review;
use crate::syncpoint;
use crate::track;

pub struct Args<'a> {
    pub update: bool,
    pub accept_growth: bool,
    pub since: Option<&'a str>,
}

/// Everything `check` found, for `check` itself and for `status`.
pub(crate) struct Evaluation {
    pub data: Value,
    pub failures: usize,
    pub improvements: bool,
    /// The single most useful fix for the first failure.
    pub next: Option<Next>,
    pub inbox: Inbox,
}

struct Budgets {
    costs: std::collections::BTreeMap<String, u64>,
    growth: Vec<String>,
    improvements: Vec<String>,
    adopted: Vec<String>,
    new_adopted: Vec<String>,
    released: Vec<String>,
    strays: Vec<String>,
    /// Unmarked changes per file now.
    unmarked: std::collections::BTreeMap<String, u64>,
    /// Files with more unmarked changes than accepted.
    unmarked_growth: Vec<String>,
    /// The first unmarked change in each grown file: (path, line).
    unmarked_at: Vec<(String, usize)>,
}

fn budgets(fork: &Fork, base: &str) -> Result<Budgets> {
    let (git, config) = (&fork.git, &fork.config);
    let upstream_ref = track::target(git, config)?.commit;
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
    let new_adopted = adopted
        .iter()
        .filter(|path| !baseline.adopted.contains(path))
        .cloned()
        .collect();
    let mut unmarked = std::collections::BTreeMap::new();
    let mut unmarked_growth = Vec::new();
    let mut unmarked_at = Vec::new();
    for file in map::scan(git, &fork.root, config, base)? {
        let count = file.unmarked.len() as u64;
        let allowed = baseline.unmarked.get(&file.path).copied().unwrap_or(0);
        if count > allowed {
            unmarked_growth.push(format!(
                "UNMARKED: {} has {count} change(s) with no `{}` reason (accepted: {allowed})",
                file.path, config.map.marker
            ));
            unmarked_at.push((file.path.clone(), file.unmarked[0].line));
        } else if count < allowed {
            improvements.push(format!("marked: {} {allowed} -> {count}", file.path));
        }
        if count > 0 {
            unmarked.insert(file.path, count);
        }
    }
    for (path, allowed) in &baseline.unmarked {
        if !unmarked.contains_key(path) && !improvements.iter().any(|i| i.contains(path.as_str())) {
            improvements.push(format!("marked: {path} {allowed} -> 0"));
        }
    }
    let released = baseline
        .adopted
        .iter()
        .filter(|path| !adopted.contains(path))
        .cloned()
        .collect();
    Ok(Budgets {
        costs,
        growth,
        improvements,
        adopted,
        new_adopted,
        released,
        strays,
        unmarked,
        unmarked_growth,
        unmarked_at,
    })
}

pub(crate) fn evaluate(fork: &Fork, since: Option<&str>) -> Result<Evaluation> {
    let (git, config) = (&fork.git, &fork.config);
    let point = syncpoint::sync_point(git, "HEAD")?;
    let base = &point.upstream;
    let b = budgets(fork, base)?;
    let drift = budget::drift(git, config, base, &track::target(git, config)?.commit)?;
    let all = decisions::load(&fork.root)?;
    let inbox = inbox::build(git, &fork.root, config)?;
    let ports = ports::audit(git, config, &all, &inbox)?;

    // Reviews: everything the sync point moved past since `since` (default:
    // the previous sync point).
    let previous = match since {
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

    // A relocation that points at a fork file that is gone can never be
    // ported to: the port audit would accept records for nothing.
    let mut relocations = Vec::new();
    for (upstream, relocation) in &config.relocations {
        for destination in &relocation.fork {
            if !fork.root.join(destination).exists() {
                relocations.push(format!("{upstream} -> {destination}"));
            }
        }
    }
    let failures = b.growth.len()
        + b.strays.len()
        + b.new_adopted.len()
        + drift.len()
        + ports.unverified.len()
        + reviews.missing.len()
        + b.unmarked_growth.len()
        + relocations.len();
    let data = json!({
        "sync_point": point,
        "budgets": { "files": b.costs.len(), "failures": b.growth, "improvements": b.improvements },
        "strays": b.strays,
        "relocations": { "missing": relocations },
        "unmarked": {
            "failures": b.unmarked_growth,
            "at": b.unmarked_at.iter().map(|(p, l)| json!({ "path": p, "line": l })).collect::<Vec<_>>(),
        },
        "adopted": { "new": b.new_adopted, "released": b.released, "count": b.adopted.len() },
        "drift": drift,
        "ports": {
            "unverified": ports.unverified,
            "acknowledged": ports.acknowledged,
            "pending": ports.pending.len(),
        },
        "reviews": { "since": previous, "required": reviews.required, "missing": reviews.missing },
    });
    let next = (failures > 0).then(|| first_fix(fork, &data, inbox.behind));
    Ok(Evaluation {
        improvements: !b.improvements.is_empty() || !b.released.is_empty(),
        data,
        failures,
        next,
        inbox,
    })
}

pub fn run(dir: &Path, args: Args) -> Result<Report> {
    let fork = Fork::open(dir)?;
    if args.update {
        return update(&fork, args.accept_growth);
    }
    let eval = evaluate(&fork, args.since)?;
    let summary = if eval.failures == 0 {
        format!(
            "All checks pass: {} diverged file(s) within budget, {} port record(s), {} review(s).",
            eval.data["budgets"]["files"],
            eval.data["ports"]["acknowledged"],
            eval.data["reviews"]["required"],
        )
    } else {
        format!("{} problem(s) found.", eval.failures)
    };
    let mut report = Report::success("check", summary, eval.data);
    if let Some(next) = eval.next {
        report = report.then(next).needs_action();
    } else if eval.improvements {
        report = report.with_next("tether check --update", "lock in the smaller budgets");
    }
    Ok(report)
}

fn update(fork: &Fork, accept_growth: bool) -> Result<Report> {
    let point = syncpoint::sync_point(&fork.git, "HEAD")?;
    let b = budgets(fork, &point.upstream)?;
    if !b.strays.is_empty() {
        return Err(Error::Refused(format!(
            "move the stray upstream file(s) first: {}",
            b.strays.join(", ")
        )));
    }
    let grew = !b.growth.is_empty() || !b.new_adopted.is_empty() || !b.unmarked_growth.is_empty();
    if grew && !accept_growth {
        return Err(Error::Refused(format!(
            "budgets would grow ({}); review it, then rerun with --accept-growth and say why in the commit",
            b.growth
                .iter()
                .chain(b.new_adopted.iter())
                .chain(b.unmarked_growth.iter())
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    if grew {
        autonomy::require(&fork.config, "accept-growth")?;
    }
    let updated = Baseline {
        files: b.costs,
        adopted: b.adopted,
        unmarked: b.unmarked,
    };
    updated.save(&fork.root)?;
    Ok(Report::success(
        "check",
        format!(
            "Updated .tether/budgets.toml: {} file(s) carry divergence.",
            updated.files.len()
        ),
        json!({ "files": updated.files.len(), "adopted": updated.adopted.len() }),
    )
    .with_next(
        "git add .tether/budgets.toml",
        "commit the new baseline with the change that caused it",
    ))
}

/// The single most useful next step for the first failing check.
fn first_fix(fork: &Fork, data: &Value, behind: usize) -> Next {
    let config = &fork.config;
    let short = |sha: &str| sha[..sha.len().min(12)].to_owned();
    if let Some(path) = data["strays"].get(0).and_then(Value::as_str) {
        return Next::of(
            NextKind::Edit,
            format!("git mv {path} {}", config.to_fork_path(path)),
            "a new upstream file landed outside the folder the fork moved it to; move it (and update any module declaration), then commit",
        );
    }
    if let Some(missing) = data["relocations"]["missing"]
        .get(0)
        .and_then(Value::as_str)
    {
        return Next::of(
            NextKind::Edit,
            "tether check",
            format!(
                "relocation {missing}: the fork file is gone; point [relocations] in .tether/config.toml at where that code lives now"
            ),
        );
    }
    if let Some(entry) = data["ports"]["unverified"].get(0) {
        let sha = short(entry["sha"].as_str().unwrap_or_default());
        let file = entry["missing"][0].as_str().unwrap_or_default();
        return decide_next(
            config,
            "port:ported",
            format!(
                "tether decide {sha} <ported|not-applicable|deferred> --port {file} --evidence \"<how you checked the fork's code>\""
            ),
            "a synced fix to a relocated file has no record of reaching the fork's code",
            &sha,
        );
    }
    if let Some(entry) = data["reviews"]["missing"].get(0) {
        let sha = short(entry["sha"].as_str().unwrap_or_default());
        let area = entry["area"].as_str().unwrap_or_default();
        return decide_next(
            config,
            &format!("review:{area}"),
            format!(
                "tether decide {sha} <adapted|already-covered|not-applicable> --review {area} --problem \"<what upstream fixed>\" --evidence \"<how you checked>\" [--dest <fork path>]"
            ),
            "a synced upstream change in a review area has no recorded review",
            &sha,
        );
    }
    if let Some(at) = data["unmarked"]["at"].get(0) {
        return super::map::mark_next(
            &config.map.marker,
            at["path"].as_str().unwrap_or_default(),
            at["line"].as_u64().unwrap_or(1) as usize,
        );
    }
    if data["drift"].get(0).is_some() && behind > 0 {
        return Next::run(
            "tether sync",
            "a file already matches upstream's latest copy; syncing moves the sync point past it",
        );
    }
    if data["drift"].get(0).is_some() {
        return Next::of(
            NextKind::AskHuman,
            "tether inbox --all",
            "a file matches upstream but the sync point is behind it; decide which commits were applied by hand, then close out",
        );
    }
    let mut next = Next::run(
        "tether check --update --accept-growth",
        "only after reviewing that the growth is a deliberate hook; say why in the commit",
    );
    if config.needs_human("accept-growth") {
        next.kind = NextKind::AskHuman;
    }
    next
}

/// A `decide` step, or a person's step when policy reserves the action.
pub(crate) fn decide_next(
    config: &crate::config::Config,
    action: &str,
    command: String,
    why: &str,
    sha: &str,
) -> Next {
    let kind = if config.needs_human(action) {
        NextKind::AskHuman
    } else {
        NextKind::Decide
    };
    Next::of(kind, command, why).with_packet(format!("tether packet {sha}"))
}
