//! Intent markers and the divergence map, generated from the code.
//!
//! Every fork change to an upstream-derived file must say why, right where it
//! is: a comment carrying the configured marker (`// tether: <reason>`, or
//! Grain's `// [GRAIN] <reason>`) on the changed lines or up to
//! `map.window` lines above them. A file that cannot hold comments (JSON, a
//! lock file) gets one recorded reason in `.tether/reasons.toml` instead.
//!
//! A change is a hunk of `git diff -U0` between upstream's copy at the sync
//! point and the fork's copy at HEAD. The map is rebuilt from scratch on
//! every run, so it cannot drift from the code the way a hand-written one
//! does.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::Serialize;

use crate::config::{Config, STATE_DIR};
use crate::error::{Error, Result};
use crate::git::Git;

pub const REASONS: &str = "reasons.toml";

/// `.tether/reasons.toml`: fork path → why the whole file differs.
pub fn load_reasons(root: &Path) -> Result<BTreeMap<String, String>> {
    let path = root.join(STATE_DIR).join(REASONS);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).map_err(|error| Error::BadConfig {
            path: format!("{STATE_DIR}/{REASONS}"),
            message: error.to_string(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error.into()),
    }
}

pub fn save_reasons(root: &Path, reasons: &BTreeMap<String, String>) -> Result<()> {
    let path = root.join(STATE_DIR).join(REASONS);
    if reasons.is_empty() {
        match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => return Ok(()),
        }
    }
    std::fs::create_dir_all(root.join(STATE_DIR))?;
    std::fs::write(path, toml::to_string(reasons).expect("strings serialise"))?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Marker {
    pub line: usize,
    pub reason: String,
}

/// One fork change no marker explains.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Change {
    /// First fork line of the change (for a pure deletion: the line after it).
    pub line: usize,
    pub added: usize,
    pub removed: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileMap {
    pub path: String,
    pub upstream_path: String,
    /// Changes (hunks) against upstream's copy at the sync point.
    pub changes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub markers: Vec<Marker>,
    /// Changes with no marker nearby (empty when `reason` covers the file).
    pub unmarked: Vec<Change>,
}

/// A hunk header's fork side: `@@ -a,b +c,d @@` → (c, d, b).
fn hunk(line: &str) -> Option<(usize, usize, usize)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let new = rest.split(" @@").next()?;
    let count = |field: &str| -> Option<(usize, usize)> {
        match field.split_once(',') {
            Some((start, len)) => Some((start.parse().ok()?, len.parse().ok()?)),
            None => Some((field.parse().ok()?, 1)),
        }
    };
    let (_, removed) = count(old)?;
    let (start, added) = count(new)?;
    Some((start, added, removed))
}

/// The reason written after the marker on a line, comment closers trimmed.
fn reason_after(line: &str, marker: &str) -> Option<String> {
    let at = line.find(marker)?;
    let text = line[at + marker.len()..]
        .trim()
        .trim_end_matches("*/")
        .trim_end_matches("-->")
        .trim();
    Some(text.to_owned())
}

/// Markers in `text`, and the changes (`diff`) with no marker within
/// `window` lines above or inside them.
pub fn analyse(
    text: &str,
    diff: &str,
    marker: &str,
    window: usize,
) -> (Vec<Marker>, Vec<Change>, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let markers: Vec<Marker> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| {
            reason_after(line, marker).map(|reason| Marker {
                line: i + 1,
                reason,
            })
        })
        .collect();
    let marked_at = |line: usize| {
        lines
            .get(line.wrapping_sub(1))
            .is_some_and(|l| l.contains(marker))
    };
    let mut unmarked = Vec::new();
    let mut changes = 0;
    for (start, added, removed) in diff.lines().filter_map(hunk) {
        changes += 1;
        // A pure deletion's start names the line *before* it.
        let (first, last) = if added == 0 {
            (start + 1, start + 1)
        } else {
            (start, start + added - 1)
        };
        let from = first.saturating_sub(window).max(1);
        if !(from..=last).any(marked_at) {
            unmarked.push(Change {
                line: first,
                added,
                removed,
            });
        }
    }
    (markers, unmarked, changes)
}

/// The divergence map: every upstream-derived fork file in scope that differs
/// from upstream's copy at the sync point `base`.
pub fn scan(git: &Git, root: &Path, config: &Config, base: &str) -> Result<Vec<FileMap>> {
    let scope = config.map_scope();
    if scope.is_empty() {
        return Ok(Vec::new());
    }
    let excluded = |path: &str| {
        config.map.exclude.iter().any(|e| e == path)
            || config.budget.exclude.iter().any(|e| e == path)
            || config.is_owned(path)
    };
    let reasons = load_reasons(root)?;
    let base_files: HashMap<String, String> = git
        .ls_tree(
            base,
            &scope
                .iter()
                .map(|p| config.to_upstream_path(p))
                .collect::<Vec<_>>(),
        )?
        .into_iter()
        .collect();
    let mut pairs = Vec::new();
    for (path, blob) in git.ls_tree("HEAD", scope)? {
        if excluded(&path) {
            continue;
        }
        let upstream_path = config.to_upstream_path(&path);
        match base_files.get(&upstream_path) {
            Some(base_blob) if *base_blob != blob => {
                pairs.push((path, upstream_path, base_blob.clone(), blob));
            }
            _ => {}
        }
    }
    let ids: Vec<&str> = pairs.iter().map(|p| p.3.as_str()).collect();
    let bodies = git.blobs(&ids)?;
    let mut files = Vec::new();
    for ((path, upstream_path, base_blob, blob), body) in pairs.into_iter().zip(bodies) {
        let diff = git.raw(&["diff", "-U0", "--no-color", &base_blob, &blob])?;
        let text = String::from_utf8_lossy(&body);
        let (markers, mut unmarked, changes) =
            analyse(&text, &diff, &config.map.marker, config.map.window);
        if changes == 0 {
            continue;
        }
        let reason = reasons.get(&path).cloned();
        if reason.is_some() {
            unmarked.clear();
        }
        files.push(FileMap {
            path,
            upstream_path,
            changes,
            reason,
            markers,
            unmarked,
        });
    }
    Ok(files)
}

/// The map as Markdown, for people (`tether map --write`).
pub fn render(files: &[FileMap], base: &str, marker: &str) -> String {
    let mut out = format!(
        "# Divergence map\n\nGenerated by `tether map` from the `{marker}` markers in the code, \
         against upstream {}. Do not edit; change the markers instead.\n",
        &base[..base.len().min(12)]
    );
    for file in files {
        out.push_str(&format!("\n## {}\n\n", file.path));
        if file.upstream_path != file.path {
            out.push_str(&format!("Upstream: `{}`. ", file.upstream_path));
        }
        out.push_str(&format!("{} change(s).\n", file.changes));
        if let Some(reason) = &file.reason {
            out.push_str(&format!("\nWhole file: {reason}\n"));
        }
        if !file.markers.is_empty() || !file.unmarked.is_empty() {
            out.push('\n');
        }
        for marker in &file.markers {
            let reason = if marker.reason.is_empty() {
                "(no reason given)"
            } else {
                &marker.reason
            };
            out.push_str(&format!("- L{}: {reason}\n", marker.line));
        }
        for change in &file.unmarked {
            out.push_str(&format!(
                "- L{}: **unmarked** (+{} −{})\n",
                change.line, change.added, change.removed
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORK: &str = "a\n// tether: keep the limit\nlimit = 9\nb\nc\nd\ne\nf\nextra\n";

    #[test]
    fn a_marker_covers_changes_just_below_it_only() {
        // Line 3 changed (marker on line 2); line 9 added far from any marker.
        let diff = "@@ -2 +2,2 @@\n-limit = 5\n+// tether: keep the limit\n+limit = 9\n@@ -7,0 +9 @@\n+extra\n";
        let (markers, unmarked, changes) = analyse(FORK, diff, "tether:", 3);
        assert_eq!(changes, 2);
        assert_eq!(
            markers,
            vec![Marker {
                line: 2,
                reason: "keep the limit".into()
            }]
        );
        assert_eq!(
            unmarked,
            vec![Change {
                line: 9,
                added: 1,
                removed: 0
            }]
        );
    }

    #[test]
    fn hunk_headers_parse() {
        assert_eq!(hunk("@@ -3,2 +4 @@ fn x()"), Some((4, 1, 2)));
        assert_eq!(hunk("@@ -10 +9,0 @@"), Some((9, 0, 1)));
        assert_eq!(
            reason_after("x /* [GRAIN] hook */", "[GRAIN]"),
            Some("hook".into())
        );
    }
}
