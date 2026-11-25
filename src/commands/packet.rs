//! `tether packet <item>`: everything needed to act on one item, in one read.
//!
//! An item is an upstream commit (short sha, `#PR` or PR number) or, while a
//! sync is in progress, a conflicted path. The packet is bounded by
//! `agent.packet_bytes`: diffs are cut at whole files and the cut files are
//! listed, so an agent knows exactly what it has not seen.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Value, json};

use super::Fork;
use super::check::decide_next;
use crate::budget::Baseline;
use crate::decisions::{self, Decision};
use crate::engine;
use crate::error::Result;
use crate::inbox::{self, Item, State};
use crate::report::{Next, Report};
use crate::session;
use crate::syncpoint;

pub fn run(dir: &Path, item: &str) -> Result<Report> {
    let fork = Fork::open(dir)?;
    if let Some(target) = engine::pending_sync(&fork.git)? {
        let path = item.trim().replace('\\', "/");
        let session = session::load(&fork.git)?;
        let known = engine::conflicted(&fork.git)?
            .into_iter()
            .chain(session.iter().flat_map(|s| s.conflicts.clone()))
            .any(|p| p == path);
        if known {
            let base = match &session {
                Some(session) => session.base.clone(),
                None => syncpoint::sync_point(&fork.git, "HEAD")?.upstream,
            };
            return conflict(&fork, &path, &base, &target);
        }
    }
    commit(&fork, item)
}

/// Cut a unified diff at whole files so it fits `budget` bytes.
/// Returns the kept text and the paths left out.
fn bounded_diff(diff: &str, budget: usize) -> (String, Vec<String>) {
    let mut kept = String::new();
    let mut omitted = Vec::new();
    let mut chunks: Vec<&str> = Vec::new();
    let mut start = 0;
    for (index, _) in diff.match_indices("diff --git ") {
        if index > start && (index == 0 || diff.as_bytes()[index - 1] == b'\n') {
            chunks.push(&diff[start..index]);
            start = index;
        }
    }
    chunks.push(&diff[start..]);
    for chunk in chunks.into_iter().filter(|c| !c.is_empty()) {
        if kept.len() + chunk.len() <= budget {
            kept.push_str(chunk);
        } else {
            let first = chunk.lines().next().unwrap_or_default();
            let path = first.rsplit_once(" b/").map_or(first, |(_, p)| p);
            omitted.push(path.to_owned());
        }
    }
    (kept, omitted)
}

fn commit(fork: &Fork, reference: &str) -> Result<Report> {
    let (git, config) = (&fork.git, &fork.config);
    let inbox = inbox::build(git, &fork.root, config)?;
    let item = inbox::resolve(&inbox.items, reference)?;
    let sha = item.sha.as_str();
    let short = &sha[..12];
    let message = git.raw(&["log", "-1", "--format=%B", sha])?;
    let parent = git.commit_sha(&format!("{sha}^"))?;

    let mut status_args = vec!["diff-tree", "-r", "-M", "--name-status", "--no-commit-id"];
    let mut diff_args = vec!["diff", "-M", "--no-color"];
    match &parent {
        Some(parent) => {
            status_args.extend([parent.as_str(), sha]);
            diff_args.extend([parent.as_str(), sha]);
        }
        None => {
            status_args.extend(["--root", sha]);
            diff_args = vec!["show", "--format=", "-M", "--no-color", sha];
        }
    }
    let names = git.raw(&status_args)?;
    let mut files = Vec::new();
    for line in names.lines().filter(|l| !l.is_empty()) {
        let mut parts = line.split('\t');
        let change = parts.next().unwrap_or_default();
        let Some(path) = parts.next_back() else {
            continue;
        };
        let fork_path = config.to_fork_path(path);
        let in_fork = git
            .run(&["cat-file", "-e", &format!("HEAD:{fork_path}")])?
            .success();
        files.push(json!({
            "change": &change[..1],
            "upstream_path": path,
            "fork_path": fork_path,
            "in_fork": in_fork,
            "owned": config.is_owned(path),
            "review_areas": config.review_areas_for(path).collect::<Vec<_>>(),
            "relocation": config.relocations.get(path),
        }));
    }

    let all = decisions::load(&fork.root)?;
    let decision = all.get(sha).cloned().unwrap_or_default();
    let related = related_ports(&all, item, sha);
    let required = required(item, &decision);

    let diff = git.raw(&diff_args)?;
    let budget = config
        .agent
        .packet_bytes
        .saturating_sub(message.len() + 4_000);
    let (diff, omitted) = bounded_diff(&diff, budget);
    let pr_url = match (&item.pr, &config.upstream.url) {
        (Some(pr), Some(url)) => Some(format!("{}/pull/{pr}", url.trim_end_matches('/'))),
        _ => None,
    };

    let data = json!({
        "kind": "commit",
        "sha": sha,
        "subject": item.subject,
        "date": item.date,
        "state": item.state,
        "applied_here": item.applied_here,
        "pr": item.pr,
        "pr_url": pr_url,
        "message": message.trim_end(),
        "files": files,
        "decision": decision,
        "related_ports": related,
        "required": required,
        "diff": diff,
        "diff_omitted": omitted,
    });

    let mut text = String::new();
    let _ = writeln!(text, "commit {sha}  [{:?}]", item.state);
    if let Some(url) = &pr_url {
        let _ = writeln!(text, "pr     {url}");
    }
    let _ = writeln!(text, "\n{}\n\nfiles:", message.trim_end());
    for file in &files {
        let _ = writeln!(
            text,
            "  {} {}{}",
            file["change"].as_str().unwrap_or_default(),
            file["fork_path"].as_str().unwrap_or_default(),
            if file["owned"] == true {
                "  (owned)"
            } else {
                ""
            }
        );
    }
    if !required.is_empty() {
        let _ = writeln!(text, "\nrecords still needed:");
        for step in &required {
            let _ = writeln!(text, "  {}", step["command"].as_str().unwrap_or_default());
        }
    }
    let _ = writeln!(text, "\n{diff}");
    if !omitted.is_empty() {
        let _ = writeln!(text, "(diff cut to fit; not shown: {})", omitted.join(", "));
    }

    let summary = format!(
        "{short} {}: {} file(s), {} record(s) still needed.",
        item.subject,
        files.len(),
        required.len()
    );
    let mut report = Report::success("packet", summary, data).with_detail(text);
    if let Some(step) = required.first() {
        let action = step["action"].as_str().unwrap_or_default();
        let command = step["command"].as_str().unwrap_or_default().to_owned();
        let why = step["why"].as_str().unwrap_or_default();
        report = report.then(decide_next(config, action, command, why, short));
    }
    Ok(report)
}

/// The `tether decide` steps this commit still needs, in order.
fn required(item: &Item, decision: &Decision) -> Vec<Value> {
    let short = &item.sha[..12];
    let mut steps = Vec::new();
    if item.state == State::Pending && item.applied_here {
        steps.push(json!({
            "action": "merged",
            "command": format!("tether decide {short} merged --note \"<where the fork applied it>\""),
            "why": "a fork commit already carries this change",
        }));
    }
    for file in item
        .ports
        .iter()
        .filter(|f| !decision.port.contains_key(*f))
    {
        steps.push(json!({
            "action": "port:ported",
            "command": format!(
                "tether decide {short} <ported|not-applicable|deferred> --port {file} --evidence \"<how you checked the fork's code>\""
            ),
            "why": "this relocated file's fix must be shown to reach the fork's code",
        }));
    }
    for area in item
        .reviews
        .iter()
        .filter(|a| !decision.review.contains_key(*a))
    {
        steps.push(json!({
            "action": format!("review:{area}"),
            "command": format!(
                "tether decide {short} <adapted|already-covered|not-applicable> --review {area} --problem \"<what upstream fixed>\" --evidence \"<how you checked>\" [--dest <fork path>]"
            ),
            "why": format!("upstream changed the `{area}` review area"),
        }));
    }
    steps
}

/// Earlier port records for the same relocated files: how they were handled.
fn related_ports(all: &decisions::Decisions, item: &Item, sha: &str) -> Vec<Value> {
    let mut found = Vec::new();
    for (other, decision) in all.iter().rev() {
        if other == sha {
            continue;
        }
        for (file, port) in &decision.port {
            if item.ports.contains(file) {
                found.push(json!({ "sha": other, "file": file, "port": port }));
            }
        }
        if found.len() >= 5 {
            break;
        }
    }
    found
}

fn conflict(fork: &Fork, path: &str, base: &str, target: &str) -> Result<Report> {
    let (git, config) = (&fork.git, &fork.config);
    let upstream_path = config.to_upstream_path(path);
    let range = format!("{base}..{target}");
    let log = git.ok(&["log", "--format=%H%x09%s", &range, "--", &upstream_path])?;
    let commits: Vec<Value> = log
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(sha, subject)| json!({ "sha": sha, "subject": subject }))
        .collect();

    let budget = config.agent.packet_bytes / 2;
    let upstream_diff = git.raw(&["diff", "--no-color", base, target, "--", &upstream_path])?;
    let (upstream_diff, _) = bounded_diff(&upstream_diff, budget);
    let base_blob = format!("{base}:{upstream_path}");
    let fork_blob = format!("HEAD:{path}");
    let fork_diff = if git.run(&["cat-file", "-e", &base_blob])?.success()
        && git.run(&["cat-file", "-e", &fork_blob])?.success()
    {
        let out = git.run(&["diff", "--no-color", &base_blob, &fork_blob])?;
        bounded_diff(&out.stdout, budget).0
    } else {
        String::new()
    };
    let markers = std::fs::read_to_string(fork.root.join(path))
        .map(|text| text.lines().filter(|l| l.starts_with("<<<<<<< ")).count())
        .unwrap_or(0);
    let budgeted = Baseline::load(&fork.root)?.files.get(path).copied();
    let conflicts = engine::conflicted(git)?;
    let still_conflicted = conflicts.iter().any(|p| p == path);

    let steps = [
        format!("Read `upstream_diff` (what upstream changed) and `fork_diff` (why the fork differs) for {path}."),
        "Edit the file so it keeps the fork's behaviour and upstream's fix; remove every conflict marker.".to_owned(),
        format!("git add {path}"),
        "If more files are listed by `tether status`, take the next; otherwise run `tether sync --continue`.".to_owned(),
    ];
    let data = json!({
        "kind": "conflict",
        "path": path,
        "upstream_path": upstream_path,
        "base": base,
        "target": target,
        "still_conflicted": still_conflicted,
        "markers": markers,
        "budget": budgeted,
        "commits": commits,
        "upstream_diff": upstream_diff,
        "fork_diff": fork_diff,
        "steps": steps,
    });

    let mut text = format!(
        "conflict {path}  (upstream: {upstream_path}, {markers} marker block(s))\n\nupstream commits:\n"
    );
    for commit in &commits {
        let _ = writeln!(
            text,
            "  {} {}",
            &commit["sha"].as_str().unwrap_or_default()[..12],
            commit["subject"].as_str().unwrap_or_default()
        );
    }
    let _ = writeln!(
        text,
        "\nupstream change:\n{upstream_diff}\nfork divergence:\n{fork_diff}\nsteps:"
    );
    for (n, step) in steps.iter().enumerate() {
        let _ = writeln!(text, "  {}. {step}", n + 1);
    }

    let summary = format!(
        "{path}: {} upstream commit(s) changed it; {}.",
        commits.len(),
        if still_conflicted {
            "still conflicted"
        } else {
            "resolved"
        }
    );
    let next = if conflicts.is_empty() {
        Next::run("tether sync --continue", "record the sync commit")
    } else {
        super::sync::resolve_next(&conflicts)
    };
    Ok(Report::success("packet", summary, data)
        .with_detail(text)
        .then(next))
}

#[cfg(test)]
mod tests {
    use super::bounded_diff;

    #[test]
    fn diffs_are_cut_at_whole_files() {
        let diff = "diff --git a/x b/x\n+one\ndiff --git a/y b/y\n+two two two two\n";
        let (kept, omitted) = bounded_diff(diff, 30);
        assert_eq!(kept, "diff --git a/x b/x\n+one\n");
        assert_eq!(omitted, vec!["y"]);
        let (kept, omitted) = bounded_diff(diff, 1000);
        assert_eq!(kept, diff);
        assert!(omitted.is_empty());
    }
}
