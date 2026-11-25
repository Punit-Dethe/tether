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

/// The fork's moved home for a conflicted path the fork deleted while
/// upstream changed it, when that home exists in the fork.
fn moved_home(git: &Git, config: &Config, path: &str) -> Result<Option<String>> {
    let home = config.to_fork_path(&config.to_upstream_path(path));
    if home == path || config.is_owned(path) {
        return Ok(None);
    }
    let stages = git.raw(&["ls-files", "--unmerged", "--", path])?;
    let has = |stage: &str| {
        stages.lines().any(|line| {
            line.split('\t')
                .next()
                .unwrap_or("")
                .ends_with(&format!(" {stage}"))
        })
    };
    if !(has("1") && has("3") && !has("2")) {
        return Ok(None);
    }
    let in_head = git
        .run(&["cat-file", "-e", &format!("HEAD:{home}")])?
        .success();
    Ok(in_head.then_some(home))
}

/// Three-way merge upstream's change to `path` into the fork's `home`, then
/// drop `path`. Returns whether the merge was clean; when it is not, `home`
/// is left as a real conflict (markers in the file, stages in the index).
fn merge_into_home(git: &Git, path: &str, home: &str) -> Result<bool> {
    let base = git.ok(&["rev-parse", &format!(":1:{path}")])?;
    let theirs = git.ok(&["rev-parse", &format!(":3:{path}")])?;
    let ours = git.ok(&["rev-parse", &format!("HEAD:{home}")])?;
    let mode = git.ok(&["ls-tree", "--format=%(objectmode)", "HEAD", "--", home])?;
    let mode = if mode.is_empty() {
        "100644".to_owned()
    } else {
        mode
    };
    let dir = git.git_path("tether-merge")?;
    std::fs::create_dir_all(&dir)?;
    let blobs = git.blobs(&[&ours, &base, &theirs])?;
    let files: Vec<PathBuf> = ["ours", "base", "theirs"]
        .iter()
        .zip(&blobs)
        .map(|(name, body)| {
            let file = dir.join(name);
            std::fs::write(&file, body).map(|_| file)
        })
        .collect::<std::io::Result<_>>()?;
    let names: Vec<String> = files
        .iter()
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    let merged = git.run_bytes(&[
        "merge-file",
        "-p",
        "-L",
        &format!("fork ({home})"),
        "-L",
        "base",
        "-L",
        &format!("upstream ({path})"),
        &names[0],
        &names[1],
        &names[2],
    ])?;
    let _ = std::fs::remove_dir_all(&dir);
    // merge-file exits with the number of conflicts (capped at 127); a
    // larger or negative status is an error.
    if !(0..=127).contains(&merged.0) {
        return Err(Error::GitFailed {
            args: format!("merge-file ({home})"),
            stderr: String::from_utf8_lossy(&merged.2).trim().to_owned(),
        });
    }
    std::fs::write(git.dir().join(home), &merged.1)?;
    git.ok(&["rm", "--force", "--quiet", "--cached", "--", path])?;
    let _ = std::fs::remove_file(git.dir().join(path));
    if merged.0 == 0 {
        git.ok(&["add", "--", home])?;
        return Ok(true);
    }
    git.ok(&["update-index", "--force-remove", "--", home])?;
    let entries =
        format!("{mode} {base} 1\t{home}\n{mode} {ours} 2\t{home}\n{mode} {theirs} 3\t{home}\n");
    git.ok_with_input(&["update-index", "--index-info"], &entries)?;
    Ok(false)
}

/// What the owned-area rules did to a fresh merge.
#[derive(Debug, Default, serde::Serialize)]
pub struct OwnedResolution {
    /// Conflicts inside an owned area, resolved to the fork's side.
    pub resolved: Vec<String>,
    /// Upstream changes to a file the fork keeps at its moved home
    /// (`path -> home`), three-way merged into that home. Rename detection
    /// misses these when the home has drifted too far from upstream.
    pub moved: Vec<String>,
    /// Moved homes the three-way merge left with conflict markers.
    pub moved_conflicts: Vec<String>,
    /// tether state files the merge tried to change (rename detection paired
    /// an upstream file with them). Kept as they were; the upstream change
    /// they carried did NOT land and must be applied by hand.
    pub state_reverted: Vec<String>,
    /// Upstream changes git routed into owned areas without a conflict
    /// (directory-rename detection), reverted.
    pub reverted: Vec<String>,
}

/// Apply the owned-area rules: upstream never wins inside an owned area.
pub fn apply_owned(git: &Git, config: &Config, target: &str) -> Result<OwnedResolution> {
    let mut outcome = OwnedResolution::default();
    for path in conflicted(git)? {
        if let Some(home) = moved_home(git, config, &path)? {
            let clean = merge_into_home(git, &path, &home)?;
            outcome.moved.push(format!("{path} -> {home}"));
            if !clean {
                outcome.moved_conflicts.push(home);
            }
            continue;
        }
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
    // tether's own state never takes upstream's edits, even clean ones that
    // rename detection routed into it.
    for dir in config.state_dirs() {
        for path in lines(&git.raw(&["diff", "--name-only", "HEAD", "--", &dir])?) {
            keep_ours(git, &path)?;
            outcome.state_reverted.push(path);
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

/// Which way [`copy_resolutions`] moves rerere entries.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CacheDirection {
    /// From the committed cache into `.git/rr-cache`: entries are inflated.
    Restore,
    /// From `.git/rr-cache` into the committed cache: entries are deflated.
    Save,
}

/// The committed cache stores each image zlib-compressed (`preimage.z`).
/// A plain preimage is a near copy of the file it came from, so git's rename
/// detection would pair an upstream file with the cache entry and merge
/// upstream's edits into the cache instead of the code. Compressed bytes are
/// never similar to source text.
const COMPRESSED: &str = ".z";

/// Copy complete rerere resolutions (entries with a postimage) between git's
/// cache and the committed one, skipping entries already there. Entries are
/// named by the conflict's hash, so nothing is ever overwritten. Returns how
/// many were copied.
pub fn copy_resolutions(
    from: &std::path::Path,
    to: &std::path::Path,
    direction: CacheDirection,
) -> Result<usize> {
    use std::io::{Read, Write};
    let Ok(entries) = std::fs::read_dir(from) else {
        return Ok(0);
    };
    let mut copied = 0;
    for entry in entries {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let complete = source.join("postimage").is_file()
            || source.join(format!("postimage{COMPRESSED}")).is_file();
        if !complete || target.exists() {
            continue;
        }
        std::fs::create_dir_all(&target)?;
        for file in std::fs::read_dir(&source)? {
            let file = file?;
            if !file.file_type()?.is_file() {
                continue;
            }
            let name = file.file_name().to_string_lossy().into_owned();
            let bytes = std::fs::read(file.path())?;
            match (direction, name.strip_suffix(COMPRESSED)) {
                (CacheDirection::Restore, Some(plain)) => {
                    let mut out = Vec::new();
                    flate2::read::ZlibDecoder::new(&bytes[..]).read_to_end(&mut out)?;
                    std::fs::write(target.join(plain), out)?;
                }
                (CacheDirection::Save, None) => {
                    let mut encoder =
                        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
                    encoder.write_all(&bytes)?;
                    std::fs::write(
                        target.join(format!("{name}{COMPRESSED}")),
                        encoder.finish()?,
                    )?;
                }
                _ => std::fs::write(target.join(&name), bytes)?,
            }
        }
        copied += 1;
    }
    Ok(copied)
}
