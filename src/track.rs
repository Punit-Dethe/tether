//! What "upstream" means right now: the branch tip, or the newest release.
//!
//! With `track = "releases"`, upstream's tags are fetched into a namespace of
//! their own (`refs/tether/tags/<remote>/`), never into `refs/tags/`, so an
//! upstream `v1.0.0` can never collide with the fork's own release tags.
//! The newest tag matching `upstream.tags` (by version order, pre-releases
//! skipped unless allowed) is the sync target. Commits on the branch past it
//! are *unreleased*: tracked for security fixes, not synced by default.

use serde::Serialize;

use crate::config::{Config, Track};
use crate::error::{Error, Result};
use crate::git::Git;
use crate::paths::glob_match;

/// Where upstream's tags are kept for `remote`.
pub fn tag_namespace(remote: &str) -> String {
    format!("refs/tether/tags/{remote}/")
}

/// Fetch upstream: the branch, and its tags into tether's own namespace.
/// Tags are needed when tracking releases; when tracking the branch they
/// only label which release first shipped each commit, so a remote that
/// refuses them is not an error there.
pub fn fetch(git: &Git, config: &Config) -> Result<()> {
    let remote = &config.upstream.remote;
    git.ok(&["fetch", "--quiet", remote])?;
    let refspec = format!("+refs/tags/*:{}*", tag_namespace(remote));
    let tags = git.ok(&["fetch", "--quiet", "--no-tags", remote, &refspec]);
    if config.upstream.track == Track::Releases {
        tags?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Release {
    pub name: String,
    pub commit: String,
}

/// Is `tag` a pre-release (`v2.0.0-rc.1`, `v2.0.0-beta`)?
fn is_prerelease(tag: &str) -> bool {
    tag.trim_start_matches(|c: char| !c.is_ascii_digit())
        .contains('-')
}

/// Upstream releases, newest first by version order.
pub fn releases(git: &Git, config: &Config) -> Result<Vec<Release>> {
    let namespace = tag_namespace(&config.upstream.remote);
    let out = git.ok(&[
        "for-each-ref",
        "--sort=-v:refname",
        "--format=%(refname)%00%(objectname)%00%(*objectname)",
        &namespace,
    ])?;
    let mut releases = Vec::new();
    for line in out.lines() {
        let mut fields = line.split('\0');
        let (Some(refname), Some(object), peeled) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let name = refname.strip_prefix(&namespace).unwrap_or(refname);
        if !glob_match(&config.upstream.tags, name)
            || (!config.upstream.prereleases && is_prerelease(name))
        {
            continue;
        }
        // An annotated tag points at a tag object; its commit is the peeled id.
        let commit = match peeled {
            Some(peeled) if !peeled.is_empty() => peeled,
            _ => object,
        };
        releases.push(Release {
            name: name.to_owned(),
            commit: commit.to_owned(),
        });
    }
    Ok(releases)
}

/// The commit a sync goes to by default, and what to call it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Target {
    /// `upstream/main`, or the release tag (`v1.4.0`).
    pub name: String,
    pub commit: String,
    /// Set when tracking releases.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
}

pub fn target(git: &Git, config: &Config) -> Result<Target> {
    let branch = config.upstream_ref();
    match config.upstream.track {
        Track::Branch => {
            let commit = git.commit_sha(&branch)?.ok_or_else(|| {
                Error::Refused(format!(
                    "{branch} is not fetched; run `git fetch {}`",
                    config.upstream.remote
                ))
            })?;
            Ok(Target {
                name: branch,
                commit,
                release: None,
            })
        }
        Track::Releases => {
            let newest = releases(git, config)?.into_iter().next().ok_or_else(|| {
                Error::Refused(format!(
                    "no upstream release tags matching `{}` are fetched; run `tether sync --dry-run` to fetch them, or set upstream.track = \"branch\"",
                    config.upstream.tags
                ))
            })?;
            Ok(Target {
                name: newest.name.clone(),
                commit: newest.commit,
                release: Some(newest.name),
            })
        }
    }
}

/// Is `sha` upstream's (on the tracked branch, or in a tracked release)?
pub fn in_upstream(git: &Git, config: &Config, sha: &str) -> Result<bool> {
    if git.is_ancestor(sha, &config.upstream_ref())? {
        return Ok(true);
    }
    if config.upstream.track == Track::Releases {
        for release in releases(git, config)? {
            if git.is_ancestor(sha, &release.commit)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::is_prerelease;

    #[test]
    fn prerelease_suffixes_are_recognised() {
        assert!(is_prerelease("v2.0.0-rc.1"));
        assert!(is_prerelease("release-2.0.0-beta"));
        assert!(!is_prerelease("v2.0.0"));
        assert!(!is_prerelease("release-2.0.0"));
    }
}
