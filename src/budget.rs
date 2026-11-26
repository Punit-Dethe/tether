//! Divergence budgets and the other tree checks.
//!
//! - **Budgets:** every fork file that still has an upstream counterpart (in
//!   the configured scope) carries a line budget: lines added + removed
//!   against the sync point. It may shrink freely and never grow without a
//!   deliberate, reviewed update. Files with no upstream counterpart are
//!   invisible: that is where fork features belong.
//! - **Strays:** a new upstream file that lands at its upstream path inside a
//!   moved folder's old location instead of the new one.
//! - **Adopted:** fork files in an owned area that became byte-identical to
//!   upstream's counterpart (upstream work arriving through rename detection).
//! - **Drift:** files identical to upstream's latest copy but billed against
//!   the sync point: applied by hand, so every sync will replay them.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::git::Git;

pub const FILE: &str = "budgets.toml";

/// `.tether/budgets.toml`: the accepted state the checks compare against.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    /// Accepted divergence per fork file, in changed lines.
    #[serde(default)]
    pub files: BTreeMap<String, u64>,
    /// Owned-area files accepted as still identical to upstream's copy.
    #[serde(default)]
    pub adopted: Vec<String>,
    /// Accepted count of unmarked fork changes per file (shrinks only).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unmarked: BTreeMap<String, u64>,
}

impl Baseline {
    pub fn load(root: &Path) -> Result<Baseline> {
        let path = root.join(crate::config::state_dir()).join(FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|error| Error::BadConfig {
                path: format!("{}/{FILE}", crate::config::state_dir()),
                message: error.to_string(),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Baseline::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let text = toml::to_string(self).expect("a baseline is always serialisable");
        std::fs::create_dir_all(root.join(crate::config::state_dir()))?;
        std::fs::write(root.join(crate::config::state_dir()).join(FILE), text)?;
        Ok(())
    }
}

/// Divergence per fork file against the sync point `base`, measured blob to
/// blob by explicit path (a tree diff would pair moved files as renames and
/// report zero). Deleted upstream files count as their full length, keyed
/// `"<upstream path> (deleted)"`.
pub fn measure(git: &Git, config: &Config, base: &str) -> Result<BTreeMap<String, u64>> {
    let scope = &config.budget.scope;
    if scope.is_empty() {
        return Ok(BTreeMap::new());
    }
    let excluded = |path: &str| config.budget.exclude.iter().any(|e| e == path);
    let base_files: HashMap<String, String> = git.ls_tree(base, scope)?.into_iter().collect();
    let ours: Vec<(String, String)> = git
        .ls_tree("HEAD", scope)?
        .into_iter()
        .filter(|(path, _)| !excluded(path))
        .collect();

    let mut costs = BTreeMap::new();
    for (path, blob) in &ours {
        let upstream_path = config.to_upstream_path(path);
        let Some(base_blob) = base_files.get(&upstream_path) else {
            continue; // fork-only file: outside the budget by design
        };
        if base_blob == blob {
            continue;
        }
        let cost = numstat(git, base_blob, blob)?;
        if cost > 0 {
            costs.insert(path.clone(), cost);
        }
    }

    let mapped: BTreeSet<String> = ours
        .iter()
        .map(|(p, _)| config.to_upstream_path(p))
        .collect();
    let mut deleted: Vec<(&String, &String)> = base_files
        .iter()
        .filter(|(path, _)| !excluded(path) && !mapped.contains(*path))
        .collect();
    deleted.sort();
    let ids: Vec<&str> = deleted.iter().map(|(_, id)| id.as_str()).collect();
    for ((path, _), body) in deleted.iter().zip(git.blobs(&ids)?) {
        costs.insert(format!("{path} (deleted)"), count_lines(&body));
    }
    Ok(costs)
}

/// Lines added + removed between two blobs (binary files count as 0).
fn numstat(git: &Git, old: &str, new: &str) -> Result<u64> {
    let out = git.ok(&["diff", "--numstat", old, new])?;
    let mut fields = out.split('\t');
    let added = fields
        .next()
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(0);
    let removed = fields
        .next()
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(0);
    Ok(added + removed)
}

/// Line count as Python's `str.splitlines()` sees it for ordinary text.
fn count_lines(body: &[u8]) -> u64 {
    String::from_utf8_lossy(body).lines().count() as u64
}

/// Upstream files sitting at their old upstream path although their folder
/// moved (git's directory-rename detection only fires for fully moved
/// folders, so a new upstream file next to files that stayed lands there).
pub fn strays(git: &Git, config: &Config, upstream_ref: &str) -> Result<Vec<String>> {
    let mut found = Vec::new();
    for moved in &config.moved {
        let prefix = vec![moved.upstream.clone()];
        let theirs: BTreeSet<String> = git
            .ls_tree(upstream_ref, &prefix)?
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        for (path, _) in git.ls_tree("HEAD", &prefix)? {
            if !path.starts_with(&moved.fork)
                && theirs.contains(&path)
                && !moved.stay.contains(&path)
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

/// Owned-area files (in a moved folder) identical to upstream's counterpart,
/// ignoring line endings.
pub fn adopted(git: &Git, config: &Config, upstream_ref: &str) -> Result<Vec<String>> {
    let areas: Vec<String> = config.owned.iter().map(|area| area.path.clone()).collect();
    if areas.is_empty() {
        return Ok(Vec::new());
    }
    let theirs: HashMap<String, String> = git.ls_tree(upstream_ref, &[])?.into_iter().collect();
    let mut same = Vec::new();
    let mut compare: Vec<(String, String, String)> = Vec::new();
    for (path, blob) in git.ls_tree("HEAD", &areas)? {
        let upstream_path = config.to_upstream_path(&path);
        if upstream_path == path {
            continue; // not moved: a shared path is a stray, not an adoption
        }
        match theirs.get(&upstream_path) {
            Some(theirs) if *theirs == blob => same.push(path),
            Some(theirs) => compare.push((path, blob, theirs.clone())),
            None => {}
        }
    }
    let ids: Vec<&str> = compare
        .iter()
        .flat_map(|(_, ours, theirs)| [ours.as_str(), theirs.as_str()])
        .collect();
    let bodies = git.blobs(&ids)?;
    for (index, (path, _, _)) in compare.iter().enumerate() {
        if same_text(&bodies[2 * index], &bodies[2 * index + 1]) {
            same.push(path.clone());
        }
    }
    same.sort();
    Ok(same)
}

fn same_text(a: &[u8], b: &[u8]) -> bool {
    let lines = |bytes: &[u8]| -> Vec<Vec<u8>> {
        bytes
            .split(|&b| b == b'\n')
            .map(|line| line.strip_suffix(b"\r").unwrap_or(line).to_vec())
            .collect()
    };
    let (mut a, mut b) = (lines(a), lines(b));
    // A trailing newline is not a line.
    if a.last().is_some_and(Vec::is_empty) {
        a.pop();
    }
    if b.last().is_some_and(Vec::is_empty) {
        b.pop();
    }
    a == b
}

/// Budgeted files identical to upstream's latest copy but not to the sync
/// point: their change was applied by hand.
pub fn drift(git: &Git, config: &Config, base: &str, upstream_ref: &str) -> Result<Vec<String>> {
    let scope = &config.budget.scope;
    if scope.is_empty() {
        return Ok(Vec::new());
    }
    let at_base: HashMap<String, String> = git.ls_tree(base, scope)?.into_iter().collect();
    let latest: HashMap<String, String> = git.ls_tree(upstream_ref, scope)?.into_iter().collect();
    let mut found = Vec::new();
    for (path, blob) in git.ls_tree("HEAD", scope)? {
        let upstream_path = config.to_upstream_path(&path);
        if latest.get(&upstream_path) == Some(&blob) && at_base.get(&upstream_path) != Some(&blob) {
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::same_text;

    #[test]
    fn line_endings_do_not_make_files_different() {
        assert!(same_text(b"a\r\nb\r\n", b"a\nb\n"));
        assert!(same_text(b"a\nb", b"a\nb\n"));
        assert!(!same_text(b"a\nb\n", b"a\nc\n"));
    }
}
