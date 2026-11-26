//! The inbox: every tracked upstream commit, its state and what it touches.
//!
//! Derived, never stored. Each run recomputes it from upstream's history,
//! the sync point and the recorded decisions, so it cannot drift.

use std::collections::{BTreeSet, HashMap, HashSet};
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
    /// On upstream's branch past the newest release (`track = "releases"`):
    /// tracked, not synced by default.
    Unreleased,
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
    /// The first upstream release that ships it (from fetched release tags).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// Why it looks like a security fix: GHSA/CVE ids, then keywords.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub security: Vec<String>,
    /// The person's own folder for it (`tether folder`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Inbox {
    pub sync_point: SyncPoint,
    /// The upstream commit the inbox was built against: the branch tip, or
    /// the newest release.
    pub target: String,
    /// The newest release, when tracking releases.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// Commits on the branch past that release.
    pub unreleased: usize,
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
    let tracked = crate::track::target(git, config)?;
    let target_ref = tracked.name.clone();
    let target = tracked.commit.clone();
    // Fetched release tags label each commit with the release that first
    // shipped it, whether the fork tracks releases or the branch.
    let releases = crate::track::releases(git, config)?;
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
    let folders = crate::folders::load(root)?;
    let applied = AppliedHere::scan(git, &point.upstream)?;

    // Releases mode: the branch past the newest release is tracked too.
    let unreleased_range = match &tracked.release {
        Some(_) => {
            let branch = config.upstream_ref();
            git.commit_sha(&branch)?
                .map(|_| format!("{target}..{branch}"))
        }
        None => None,
    };
    // Newest first, so equal timestamps keep upstream's order.
    let mut ranges: Vec<String> = unreleased_range.iter().cloned().collect();
    ranges.push(walk.clone());
    let unreleased: HashSet<String> = match &unreleased_range {
        Some(range) => git.rev_list(range)?.into_iter().collect(),
        None => HashSet::new(),
    };
    let mut messages = HashMap::new();
    for range in &ranges {
        messages.extend(bodies(git, range)?);
    }
    let shipped_in = first_release(git, &releases, &walk)?;

    let mut log = String::new();
    for range in &ranges {
        log.push_str(&git.raw(&[
            "log",
            range,
            "--format=%x00%H%x00%ct%x00%cI%x00%s",
            "--name-only",
        ])?);
        log.push('\n');
    }
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
        let mut entry = item(
            config, &decisions, &absorbed, &applied, sha, timestamp, date, subject, &paths,
        );
        if unreleased.contains(sha) && entry.state == State::Pending {
            entry.state = State::Unreleased;
            entry.applied_here = false;
        }
        entry.release = shipped_in.get(sha).cloned();
        entry.folder = folders.of(sha).map(str::to_owned);
        let body = messages.get(sha).map_or("", String::as_str);
        entry.security = security_signals(subject, body, &config.security.keywords);
        items.push(entry);
    }
    items.sort_by_key(|item| std::cmp::Reverse(item.timestamp));
    Ok(Inbox {
        sync_point: point,
        target,
        release: tracked.release.is_some().then(|| target_ref.clone()),
        unreleased: unreleased.len(),
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
        release: None,
        security: Vec::new(),
        folder: None,
    }
}

/// Full commit messages in `range`, by sha.
fn bodies(git: &Git, range: &str) -> Result<HashMap<String, String>> {
    let log = git.raw(&["log", range, "--format=%H%x00%B%x01"])?;
    Ok(log
        .split('\x01')
        .filter_map(|record| {
            let (sha, body) = record.trim_start().split_once('\0')?;
            Some((sha.to_owned(), body.to_owned()))
        })
        .collect())
}

/// The first release (oldest by version order) that contains each commit
/// the inbox walks.
fn first_release(
    git: &Git,
    releases: &[crate::track::Release],
    walk: &str,
) -> Result<HashMap<String, String>> {
    let mut shipped = HashMap::new();
    let floor = walk.split_once("..").map(|(floor, _)| format!("^{floor}"));
    for release in releases.iter().rev() {
        let mut args = vec!["rev-list", release.commit.as_str()];
        if let Some(floor) = &floor {
            args.push(floor);
        }
        for sha in git.raw(&args)?.lines() {
            shipped
                .entry(sha.to_owned())
                .or_insert_with(|| release.name.clone());
        }
    }
    Ok(shipped)
}

/// Why a commit looks like a security fix: advisory ids anywhere in its
/// message (`GHSA-xxxx-xxxx-xxxx`, `CVE-2026-1234`), then whole-word
/// `keywords` in its subject.
pub fn security_signals(subject: &str, body: &str, keywords: &[String]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let upper = format!("{subject}\n{body}").to_ascii_uppercase();
    type Valid = fn(&str) -> bool;
    let checks: [(&str, Valid); 2] = [("GHSA-", is_ghsa), ("CVE-", is_cve)];
    for (prefix, valid) in checks {
        for (at, _) in upper.match_indices(prefix) {
            if at > 0 && upper.as_bytes()[at - 1].is_ascii_alphanumeric() {
                continue;
            }
            let id: String = upper[at..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            let id = id.trim_end_matches('-').to_owned();
            if valid(&id) && !found.contains(&id) {
                found.push(id);
            }
        }
    }
    let lower = subject.to_ascii_lowercase();
    for keyword in keywords {
        let keyword = keyword.to_ascii_lowercase();
        let whole_word = lower.match_indices(keyword.as_str()).any(|(at, _)| {
            let before = lower[..at].chars().next_back();
            let after = lower[at + keyword.len()..].chars().next();
            !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
        });
        if whole_word {
            found.push(format!("keyword: {keyword}"));
        }
    }
    found
}

/// `GHSA-` and three groups of four letters or digits.
fn is_ghsa(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 4
        && groups[1..]
            .iter()
            .all(|g| g.len() == 4 && g.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// `CVE-YYYY-NNNN` (four or more digits at the end).
fn is_cve(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 3
        && groups[1].len() == 4
        && groups[2].len() >= 4
        && groups[1..]
            .iter()
            .all(|g| g.chars().all(|c| c.is_ascii_digit()))
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

/// Find one tracked item by short sha, `#PR` or bare PR number.
pub fn resolve<'a>(items: &'a [Item], reference: &str) -> Result<&'a Item> {
    let reference = reference.trim();
    let pr = reference.strip_prefix('#').or_else(|| {
        (reference.len() <= 6 && reference.bytes().all(|b| b.is_ascii_digit())).then_some(reference)
    });
    let hits: Vec<&Item> = match pr {
        Some(pr) => items
            .iter()
            .filter(|i| i.pr.as_deref() == Some(pr))
            .collect(),
        None => {
            let prefix = reference.to_ascii_lowercase();
            items
                .iter()
                .filter(|i| i.sha.starts_with(&prefix))
                .collect()
        }
    };
    match hits[..] {
        [one] => Ok(one),
        [] => Err(Error::Refused(format!(
            "no tracked upstream commit matches `{reference}`"
        ))),
        _ => Err(Error::Refused(format!(
            "`{reference}` is ambiguous: {}",
            hits.iter()
                .map(|i| &i.sha[..10])
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_signals_skip_lookalikes() {
        let words = crate::config::Security::default().keywords;
        let none = |subject: &str, body: &str| security_signals(subject, body, &words).is_empty();
        assert!(none("Make the insecure warning clearer", ""));
        assert!(none("Rename securityContext helper", ""));
        assert!(none("Bump to CVE-12 style ids", "see XCVE-2026-12345"));
        assert!(none("Tidy the ghsa-abc docs", "GHSA-abc-def"));
        assert_eq!(
            security_signals(
                "Harden auth (#1)",
                "Fixes cve-2026-1234 and CVE-2026-1234.",
                &words
            ),
            vec!["CVE-2026-1234"]
        );
        assert_eq!(
            security_signals("Prevent path traversal in export", "", &words),
            vec!["keyword: path traversal"]
        );
    }

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
