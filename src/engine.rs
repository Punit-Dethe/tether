//! Merge mode: one squashed, single-parent sync commit per sync.
//!
//! A sync is a true three-way merge against the sync point. To get git's full
//! merge machinery (rename detection, rerere, merge drivers) against a base
//! the fork's history does not contain, HEAD is grafted onto the base with a
//! temporary `git replace --graft` for the one `git merge --squash` call. The
//! replacement is deleted before returning and never reaches a commit: the
//! sync commit's only parent is HEAD's real sha.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::git::Git;
use crate::syncpoint::{self, marker_line};

/// Paths git left conflicted.
pub fn conflicted(git: &Git) -> Result<Vec<String>> {
    Ok(lines(&git.raw(&[
        "diff",
        "--name-only",
        "--diff-filter=U",
    ])?))
}

fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

pub fn in_merge(git: &Git) -> Result<bool> {
    Ok(git
        .run(&["rev-parse", "--quiet", "--verify", "MERGE_HEAD"])?
        .success())
}

pub fn dirty(git: &Git) -> Result<bool> {
    Ok(!git.ok(&["status", "--porcelain"])?.is_empty())
}

pub fn squash_msg_path(git: &Git) -> Result<PathBuf> {
    git.git_path("SQUASH_MSG")
}

/// The upstream sha of a sync that is prepared but not committed.
pub fn pending_sync(git: &Git) -> Result<Option<String>> {
    match std::fs::read_to_string(squash_msg_path(git)?) {
        Ok(body) => Ok(syncpoint::parse_markers(&body).pop()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// The sync commit's message: subject, the upstream subjects it brings in
/// (credited by link, never by authorship) and the marker.
pub fn sync_message(git: &Git, config: &Config, base: &str, target: &str) -> Result<String> {
    let range = format!("{base}..{target}");
    let log = git.raw(&["log", "--no-merges", "--format=  %h %s", &range])?;
    let count = git.ok(&["rev-list", "--count", &range])?;
    let name = config.upstream_name();
    let source = match &config.upstream.url {
        Some(url) => format!("{name} ({url})"),
        None => name.to_owned(),
    };
    Ok(format!(
        "sync(upstream): {name} {} ({count} commits)\n\n\
         Brings in {source} changes {}..{}:\n\n{}\n\n{}\n",
        &target[..8],
        &base[..12],
        &target[..12],
        log.trim_end(),
        marker_line(target)?
    ))
}

/// `git merge --squash target`, three-way against `base`. Returns git's exit
/// status: 0 clean, 1 conflicts, anything else a failure.
pub fn squash_merge(
    git: &Git,
    settings: &[String],
    base: &str,
    target: &str,
) -> Result<crate::git::Output> {
    let head = git.ok(&["rev-parse", "HEAD"])?;
    let grafted = !git.is_ancestor(base, &head)?;
    if grafted {
        let parents_line = git.ok(&["rev-list", "--parents", "-n", "1", &head])?;
        let mut args = vec!["replace", "-f", "--graft", head.as_str()];
        args.extend(parents_line.split_whitespace().skip(1));
        args.push(base);
        let graft = git.run(&args)?;
        if !graft.success() {
            return Err(Error::GitFailed {
                args: "replace --graft".into(),
                stderr: graft.stderr.trim().to_owned(),
            });
        }
    }
    let mut args: Vec<&str> = Vec::new();
    for setting in settings {
        args.extend(["-c", setting.as_str()]);
    }
    args.extend(["merge", "--squash", target]);
    let merged = git.run(&args);
    if grafted {
        let _ = git.run(&["replace", "-d", &head]);
    }
    merged
}

/// Resolve a path to the fork's version: restore it from HEAD, or remove it
/// when the fork does not have it.
fn keep_ours(git: &Git, path: &str) -> Result<()> {
    let spec = format!("HEAD:{path}");
    if git.run(&["cat-file", "-e", &spec])?.success() {
        git.ok(&["checkout", "HEAD", "--", path])?;
    } else {
        git.ok(&["rm", "--force", "--quiet", "--", path])?;
    }
    Ok(())
}

/// What the owned-area rules did to a fresh merge.
#[derive(Debug, Default, serde::Serialize)]
pub struct OwnedResolution {
    /// Conflicts inside an owned area, resolved to the fork's side.
    pub resolved: Vec<String>,
    /// Upstream changes git routed into owned areas without a conflict
    /// (directory-rename detection), reverted.
    pub reverted: Vec<String>,
}

/// Apply the owned-area rules: upstream never wins inside an owned area.
pub fn apply_owned(git: &Git, config: &Config, target: &str) -> Result<OwnedResolution> {
    let mut outcome = OwnedResolution::default();
    for path in conflicted(git)? {
        // A relocated upstream file the fork deleted: its fixes reach the
        // fork by porting, which the port audit enforces, never by merge.
        let relocated_away = config
            .relocations
            .contains_key(&config.to_upstream_path(&path))
            && !git
                .run(&["cat-file", "-e", &format!("HEAD:{path}")])?
                .success();
        if config.is_owned(&path) || relocated_away {
            keep_ours(git, &path)?;
            outcome.resolved.push(path);
        }
    }
    // A change counts as adopted when the merge touched the path AND upstream
    // has the matching file: that pairing is exactly what rename detection
    // used to place it.
    for area in &config.owned {
        let changed = lines(&git.raw(&["diff", "--name-only", "HEAD", "--", &area.path])?);
        for path in changed {
            let upstream_path = config.to_upstream_path(&path);
            let spec = format!("{target}:{upstream_path}");
            if git.run(&["cat-file", "-e", &spec])?.success() {
                keep_ours(git, &path)?;
                outcome.reverted.push(path);
            }
        }
    }
    Ok(outcome)
}

/// Write the prepared commit message where `git commit` and tether look.
pub fn write_squash_msg(git: &Git, message: &str) -> Result<()> {
    std::fs::write(squash_msg_path(git)?, message)?;
    Ok(())
}

pub fn remove_squash_msg(git: &Git) -> Result<()> {
    match std::fs::remove_file(squash_msg_path(git)?) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

/// Commit the prepared sync as one single-parent commit.
pub fn commit_sync(git: &Git, settings: &[String]) -> Result<String> {
    let message: PathBuf = squash_msg_path(git)?;
    let message = message.to_string_lossy().into_owned();
    // --allow-empty: a sync whose every change was declined still moves the
    // sync point; that review happened.
    // The merge's settings again: rerere records resolutions at commit time.
    let mut args: Vec<&str> = Vec::new();
    for setting in settings {
        args.extend(["-c", setting.as_str()]);
    }
    args.extend(["commit", "--allow-empty", "-q", "-F", &message]);
    git.ok(&args)?;
    remove_squash_msg(git)?;
    git.ok(&["rev-parse", "HEAD"])
}

/// Every upstream commit in `base..head` and the paths it touched, merges
/// included (a merge's paths are the union over its parents).
pub fn touched_in_range(git: &Git, base: &str, head: &str) -> Result<Vec<(String, Vec<String>)>> {
    let range = format!("{base}..{head}");
    let log = git.raw(&[
        "log",
        "--reverse",
        "-m",
        "--name-only",
        "--format=%x00%H",
        &range,
    ])?;
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for line in log.lines() {
        if let Some(sha) = line.strip_prefix('\0') {
            if out.last().is_none_or(|(last, _)| last != sha) {
                out.push((sha.to_owned(), Vec::new()));
            }
        } else if let Some((_, paths)) = out.last_mut() {
            let path = line.trim();
            if !path.is_empty() && !paths.iter().any(|p| p == path) {
                paths.push(path.to_owned());
            }
        }
    }
    Ok(out)
}

pub fn repo_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Copy complete rerere resolutions (entries with a `postimage`) from `from`
/// to `to`, skipping ones already there. Entries are named by the conflict's
/// hash, so nothing is ever overwritten. Returns how many were copied.
pub fn copy_resolutions(from: &std::path::Path, to: &std::path::Path) -> Result<usize> {
    let Ok(entries) = std::fs::read_dir(from) else {
        return Ok(0);
    };
    let mut copied = 0;
    for entry in entries {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        if !source.join("postimage").is_file() || target.exists() {
            continue;
        }
        std::fs::create_dir_all(&target)?;
        for file in std::fs::read_dir(&source)? {
            let file = file?;
            if file.file_type()?.is_file() {
                std::fs::copy(file.path(), target.join(file.file_name()))?;
            }
        }
        copied += 1;
    }
    Ok(copied)
}
