//! Where the fork last synced, read from commit-message markers.
//!
//! The fork shares no history with upstream. Each sync lands as one ordinary
//! commit whose message ends with
//!
//! ```text
//! Upstream-Synced: <full 40-char upstream sha>
//! ```
//!
//! "Is this upstream commit already in the fork?" becomes "is it an ancestor
//! of the sync point in upstream's own history?". The latest sync point is
//! decided in upstream's history, not by date: it must contain every other
//! recorded one. Markers that diverge are refused, never guessed at.

use std::collections::HashSet;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::git::Git;

pub const MARKER: &str = "Upstream-Synced";

/// The line a sync or close-out commit carries.
pub fn marker_line(sha: &str) -> Result<String> {
    if !is_full_sha(sha) {
        return Err(Error::Refused(format!(
            "a sync marker needs a full 40-character sha, got {sha:?}"
        )));
    }
    Ok(format!("{MARKER}: {}", sha.to_ascii_lowercase()))
}

fn is_full_sha(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The upstream shas a commit message records, in order. The key is matched
/// case-insensitively and may be followed by spaces or tabs, as git trailers
/// are written by hand.
pub fn parse_markers(message: &str) -> Vec<String> {
    let key = format!("{}:", MARKER.to_ascii_lowercase());
    message
        .split('\n')
        .filter_map(|line| {
            let head = line.get(..key.len())?;
            if !head.eq_ignore_ascii_case(&key) {
                return None;
            }
            let value = line[key.len()..].trim_matches([' ', '\t']);
            is_full_sha(value).then(|| value.to_ascii_lowercase())
        })
        .collect()
}

/// Every (fork commit, upstream sha) marker reachable from `rev`, newest first.
pub fn markers(git: &Git, rev: &str) -> Result<Vec<(String, String)>> {
    let grep = format!("--grep=^{MARKER}:");
    let out = git.raw(&["log", rev, "-E", &grep, "--format=%H%x00%B%x01"])?;
    let mut found = Vec::new();
    for record in out.split('\x01') {
        let record = record.trim_matches('\n');
        let Some((commit, body)) = record.split_once('\0') else {
            continue;
        };
        let commit = commit.trim();
        if commit.is_empty() {
            continue;
        }
        found.extend(
            parse_markers(body)
                .into_iter()
                .map(|sha| (commit.to_owned(), sha)),
        );
    }
    Ok(found)
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SyncPoint {
    /// The fork commit that recorded it.
    pub commit: String,
    /// The upstream commit the fork contains everything up to.
    pub upstream: String,
}

/// The latest sync reachable from `rev`.
pub fn sync_point(git: &Git, rev: &str) -> Result<SyncPoint> {
    let found = markers(git, rev)?;
    if found.is_empty() {
        return Err(Error::NoSyncPoint(rev.to_owned()));
    }
    // Newest fork commit for each upstream sha, keeping first-seen order.
    let mut unique: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();
    for (commit, sha) in found {
        if seen.insert(sha.clone()) {
            unique.push((sha, commit));
        }
    }
    for (sha, _) in &unique {
        if !git.has_commit(sha)? {
            return Err(Error::SyncPointMissing(sha[..12].to_owned()));
        }
    }
    let mut best = 0;
    for index in 1..unique.len() {
        if git.is_ancestor(&unique[best].0, &unique[index].0)? {
            best = index;
        }
    }
    for (index, (sha, _)) in unique.iter().enumerate() {
        if index != best && !git.is_ancestor(sha, &unique[best].0)? {
            return Err(Error::SyncPointsDiverge {
                best: unique[best].0[..12].to_owned(),
                stray: sha[..12].to_owned(),
            });
        }
    }
    let (upstream, commit) = unique.swap_remove(best);
    Ok(SyncPoint { commit, upstream })
}

/// Every upstream commit the fork already contains at `rev`.
pub fn absorbed(git: &Git, rev: &str) -> Result<HashSet<String>> {
    let point = sync_point(git, rev)?;
    Ok(git.rev_list(&point.upstream)?.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn reads_markers_the_way_people_write_them() {
        let msg = format!(
            "sync\n\nUpstream-Synced: {A}\nupstream-synced:\t{}  \n",
            A.to_uppercase()
        );
        assert_eq!(parse_markers(&msg), vec![A.to_owned(), A.to_owned()]);
    }

    #[test]
    fn ignores_short_shas_and_prose() {
        let msg = "Upstream-Synced: 0123456\nsee Upstream-Synced: in docs\n";
        assert!(parse_markers(msg).is_empty());
        assert!(parse_markers(&format!("x Upstream-Synced: {A}")).is_empty());
    }

    #[test]
    fn marker_line_needs_a_full_sha() {
        assert_eq!(marker_line(A).unwrap(), format!("Upstream-Synced: {A}"));
        assert!(marker_line("abc").is_err());
    }
}
