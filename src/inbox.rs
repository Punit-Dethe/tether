//! The inbox: every tracked upstream commit, its state and what it touches.
//!
//! Derived, never stored. Each run recomputes it from upstream's history,
//! the sync point and the recorded decisions, so it cannot drift.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use serde::Serialize;

use crate::config::Config;
use crate::decisions::{self, Decisions, Status};
use crate::error::{Error, Result};
use crate::git::Git;
use crate::syncpoint::{self, SyncPoint};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// Past the sync point, no decision yet.
    Pending,
    /// In the fork: at or before the sync point, or recorded as applied.
    Merged,
    /// Deliberately not taken.
    Ignored,
}

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub sha: String,
    pub subject: String,
    /// Committer date, ISO 8601.
    pub date: String,
    #[serde(skip)]
    pub timestamp: i64,
    /// The pull request that landed it, from a trailing `(#123)`.
    pub pr: Option<String>,
    pub state: State,
    /// At or before the sync point (what the state would be with no decision).
    #[serde(skip)]
    pub absorbed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Relocated upstream files it touched (each needs a port record).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<String>,
    /// Review areas it touched (each needs a review record).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reviews: Vec<String>,
    /// Pending, but a fork commit already carries its subject or an
    /// `Upstream-Commit:` trailer: probably applied by hand.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub applied_here: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Inbox {
    pub sync_point: SyncPoint,
    /// The upstream branch tip the inbox was built against.
    pub target: String,
    /// Upstream commits past the sync point.
    pub behind: usize,
    pub items: Vec<Item>,
}

impl Inbox {
    pub fn count(&self, state: State) -> usize {
        self.items.iter().filter(|item| item.state == state).count()
    }
}

/// Build the inbox for the fork at `root`.
pub fn build(git: &Git, root: &Path, config: &Config) -> Result<Inbox> {
    let target_ref = config.upstream_ref();
    let target = git.commit_sha(&target_ref)?.ok_or_else(|| {
        Error::Refused(format!(
            "{target_ref} is not fetched; run `git fetch {}`",
            config.upstream.remote
        ))
    })?;
    let point = syncpoint::sync_point(git, "HEAD")?;
    if !git.is_ancestor(&point.upstream, &target)? {
        return Err(Error::Refused(format!(
            "{target_ref} does not contain the sync point {}",
            &point.upstream[..12]
        )));
    }
    let range = format!("{}..{target}", point.upstream);
    let behind: usize = git
        .ok(&["rev-list", "--count", &range])?
        .parse()
        .unwrap_or(0);

    let floor = config
        .upstream
        .since
        .as_deref()
        .and_then(crate::time::parse_utc);
    // Without a date, tracking starts at the first sync point the fork ever
    // recorded: everything after it was synced (and must stay auditable) or
    // is still pending.
    let walk = if floor.is_some() {
        target.clone()
    } else {
        let first = syncpoint::first_sync_point(git, "HEAD")?;
        format!("{first}..{target}")
    };
    let absorbed: HashSet<String> = git.rev_list(&point.upstream)?.into_iter().collect();
    let decisions = decisions::load(root)?;
    let applied = AppliedHere::scan(git, &point.upstream)?;

    let log = git.raw(&[
        "log",
        &walk,
        "--format=%x00%H%x00%ct%x00%cI%x00%s",
        "--name-only",
    ])?;
    let mut items = Vec::new();
    for (header, paths) in records(&log) {
        let fields: Vec<&str> = header.splitn(4, '\0').collect();
        let [sha, ct, date, subject] = fields[..] else {
            continue;
        };
        let timestamp: i64 = ct.parse().unwrap_or(0);
        if floor.is_some_and(|floor| timestamp < floor) {
            continue;
        }
        items.push(item(
            config, &decisions, &absorbed, &applied, sha, timestamp, date, subject, &paths,
        ));
    }
    items.sort_by_key(|item| std::cmp::Reverse(item.timestamp));
    Ok(Inbox {
        sync_point: point,
        target,
        behind,
        items,
    })
}

#[allow(clippy::too_many_arguments)]
fn item(
    config: &Config,
    decisions: &Decisions,
    absorbed: &HashSet<String>,
    applied: &AppliedHere,
    sha: &str,
    timestamp: i64,
    date: &str,
    subject: &str,
    paths: &[&str],
) -> Item {
    let decision = decisions.get(sha);
    let state = match decision.and_then(|d| d.status) {
        Some(Status::Merged) => State::Merged,
        Some(Status::Ignored) => State::Ignored,
        None if absorbed.contains(sha) => State::Merged,
        None => State::Pending,
    };
    let mut ports = BTreeSet::new();
    let mut reviews = BTreeSet::new();
    for path in paths {
        if config.relocations.contains_key(*path) {
            ports.insert((*path).to_owned());
        }
        reviews.extend(config.review_areas_for(path).map(str::to_owned));
    }
    Item {
        sha: sha.to_owned(),
        subject: subject.to_owned(),
        date: date.to_owned(),
        timestamp,
        pr: last_pr(subject),
        state,
        absorbed: absorbed.contains(sha),
        note: decision.and_then(|d| d.note.clone()),
        ports: ports.into_iter().collect(),
        reviews: reviews.into_iter().collect(),
        applied_here: state == State::Pending && applied.matches(sha, subject),
    }
}

/// Split `git log --format=%x00… --name-only` output into (header, paths).
fn records(log: &str) -> Vec<(&str, Vec<&str>)> {
    let mut out: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in log.lines() {
        if let Some(header) = line.strip_prefix('\0') {
            out.push((header, Vec::new()));
        } else if let Some((_, paths)) = out.last_mut() {
            let path = line.trim();
            if !path.is_empty() {
                paths.push(path);
            }
        }
    }
    out
}

/// The last `(#123)` in a subject: backports carry both numbers and the last
/// is the pull request that landed it.
pub fn last_pr(subject: &str) -> Option<String> {
    pr_spans(subject)
        .last()
        .map(|(start, end)| subject[start + 2..end - 1].to_owned())
}

/// Byte ranges of every `(#digits)` in `text`.
fn pr_spans(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"(#") {
            let digits = bytes[i + 2..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            let close = i + 2 + digits;
            if digits > 0 && bytes.get(close) == Some(&b')') {
                spans.push((i, close + 1));
                i = close + 1;
                continue;
            }
        }
        i += 1;
    }
    spans
}

/// A subject as a join key: PR numbers and backticks removed, lowercased.
pub fn normalize_subject(subject: &str) -> String {
    let mut text = String::with_capacity(subject.len());
    let mut last = 0;
    for (start, end) in pr_spans(subject) {
        text.push_str(&subject[last..start]);
        last = end;
    }
    text.push_str(&subject[last..]);
    text.replace('`', "").trim().to_lowercase()
}

/// What the fork's own history says it already applied by hand.
struct AppliedHere {
    subjects: HashSet<String>,
    trailers: Vec<String>,
}

impl AppliedHere {
    /// Scan the fork's own commits: not merges (a generic "Merge branch"
    /// subject proves nothing) and not upstream's commits, which a fork that
    /// once shared history may still carry.
    fn scan(git: &Git, sync_point: &str) -> Result<Self> {
        let not_upstream = format!("^{sync_point}");
        let log = git.raw(&[
            "log",
            "HEAD",
            &not_upstream,
            "--no-merges",
            "--format=%s%x00%B%x01",
        ])?;
        let mut subjects = HashSet::new();
        let mut trailers = Vec::new();
        for record in log.split('\x01') {
            let Some((subject, body)) = record.trim_start_matches('\n').split_once('\0') else {
                continue;
            };
            subjects.insert(normalize_subject(subject));
            for line in body.lines() {
                let line = line.trim();
                let Some(value) = line
                    .get(..16)
                    .filter(|head| head.eq_ignore_ascii_case("upstream-commit:"))
                    .map(|_| line[16..].trim())
                else {
                    continue;
                };
                let ok =
                    (7..=40).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit());
                if ok {
                    trailers.push(value.to_ascii_lowercase());
                }
            }
        }
        Ok(AppliedHere { subjects, trailers })
    }

    fn matches(&self, sha: &str, subject: &str) -> bool {
        self.subjects.contains(&normalize_subject(subject))
            || self
                .trailers
                .iter()
                .any(|trailer| sha.starts_with(trailer.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_landing_pull_request() {
        assert_eq!(
            last_pr("fix prompt (#1261) (#1310)").as_deref(),
            Some("1310")
        );
        assert_eq!(last_pr("no pr here"), None);
        assert_eq!(last_pr("odd (#) and (#12"), None);
    }

    #[test]
    fn normalizes_subjects_for_matching() {
        assert_eq!(normalize_subject("Fix `foo` crash (#12) "), "fix foo crash");
        assert_eq!(normalize_subject("A (#1)(#2)"), "a");
    }
}
