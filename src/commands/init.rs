//! `tether init`: write `.tether/config.toml`, optionally record the sync point.

use std::path::Path;

use serde_json::json;

use crate::config::{Config, Upstream};
use crate::error::{Error, Result};
use crate::git::Git;
use crate::report::Report;
use crate::syncpoint;

/// The agent loop, written to `.tether/AGENT.md` by `tether init`.
pub const AGENT_GUIDE: &str = include_str!("../../AGENT.md");

pub fn run(dir: &Path, remote: &str, sync_point: Option<&str>) -> Result<Report> {
    let root = Git::new(dir).toplevel()?;
    let git = Git::new(&root);
    if !git.remotes()?.iter().any(|name| name == remote) {
        return Err(Error::Refused(format!(
            "no remote named `{remote}`; add it with `git remote add {remote} <url>`"
        )));
    }

    let wrote_config = !Config::exists(&root);
    let config = if wrote_config {
        let config = detect(&git, remote)?;
        let path = Config::path(&root);
        std::fs::create_dir_all(path.parent().expect("config has a parent"))?;
        std::fs::write(&path, config.to_toml())?;
        config
    } else {
        Config::load(&root)?
    };

    // The agent loop, next to the config, so any agent working here finds it.
    let guide = Config::path(&root).with_file_name("AGENT.md");
    if !guide.exists() {
        std::fs::create_dir_all(guide.parent().expect("guide has a parent"))?;
        std::fs::write(&guide, AGENT_GUIDE)?;
    }

    let recorded = match sync_point {
        Some(rev) => Some(record_sync_point(&git, &config, rev)?),
        None => None,
    };

    let current = match syncpoint::sync_point(&git, "HEAD") {
        Ok(point) => Some(point),
        Err(Error::NoSyncPoint(_)) => None,
        Err(error) => return Err(error),
    };
    let summary = match (wrote_config, &recorded) {
        (true, _) => "Wrote .tether/config.toml.".to_owned(),
        (false, Some(_)) => "Recorded the sync point.".to_owned(),
        (false, None) => "tether was already set up; nothing changed.".to_owned(),
    };
    let data = json!({
        "config": ".tether/config.toml",
        "wrote_config": wrote_config,
        "upstream": config.upstream,
        "recorded_sync_point": recorded,
        "sync_point": current,
    });
    let report = Report::success("init", summary, data);
    Ok(if current.is_none() {
        report
            .with_next(
                "tether init --sync-point <upstream-commit>",
                "name the upstream commit this fork already contains everything up to",
            )
            .needs_action()
    } else {
        report.with_next("tether status", "see what upstream has done since")
    })
}

/// Read what git already knows about the upstream remote.
fn detect(git: &Git, remote: &str) -> Result<Config> {
    let head = format!("refs/remotes/{remote}/HEAD");
    let branch = git
        .run(&["symbolic-ref", "--short", &head])?
        .stdout
        .trim()
        .strip_prefix(&format!("{remote}/"))
        .map(str::to_owned)
        .unwrap_or_else(|| "main".to_owned());
    let url = git
        .ok(&["remote", "get-url", remote])
        .ok()
        .and_then(|url| web_url(&url));
    let name = url
        .as_deref()
        .and_then(|url| url.rsplit('/').next())
        .map(str::to_owned);
    Ok(Config {
        upstream: Upstream {
            remote: remote.to_owned(),
            branch,
            name,
            url,
            since: None,
        },
        moved: Vec::new(),
        owned: Vec::new(),
        reviews: Vec::new(),
        relocations: Default::default(),
        budget: Default::default(),
        port_audit: Default::default(),
        agent: Default::default(),
        verify: Default::default(),
        map: Default::default(),
    })
}

/// `git@github.com:o/r.git` or `https://github.com/o/r.git` → `https://github.com/o/r`.
/// Local paths and other hosts are not web pages, so they give `None`.
fn web_url(remote_url: &str) -> Option<String> {
    let trimmed = remote_url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    if let Some(rest) = trimmed.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        return Some(format!("https://{host}/{path}"));
    }
    trimmed.starts_with("https://").then(|| trimmed.to_owned())
}

/// Record `rev` as the sync point with an empty marker commit.
fn record_sync_point(git: &Git, config: &Config, rev: &str) -> Result<String> {
    let sha = git
        .commit_sha(rev)?
        .ok_or_else(|| Error::Refused(format!("not a commit: {rev}")))?;
    let upstream = config.upstream_ref();
    if !git.is_ancestor(&sha, &upstream)? {
        return Err(Error::Refused(format!(
            "{rev} is not in {upstream}; fetch it or pick an upstream commit"
        )));
    }
    match syncpoint::sync_point(git, "HEAD") {
        Ok(point) if point.upstream == sha => return Ok(sha),
        Ok(point) if git.is_ancestor(&sha, &point.upstream)? => {
            return Err(Error::Refused(format!(
                "the fork already records a later sync point, {}",
                &point.upstream[..12]
            )));
        }
        Ok(_) | Err(Error::NoSyncPoint(_)) => {}
        Err(error) => return Err(error),
    }
    if !git.run(&["diff", "--cached", "--quiet"])?.success() {
        return Err(Error::Refused(
            "staged changes would be swept into the sync-point commit; commit or unstage them first".into(),
        ));
    }
    let subject = format!(
        "chore(upstream): record the {} sync point",
        config.upstream_name()
    );
    let line = syncpoint::marker_line(&sha)?;
    git.ok(&["commit", "--allow-empty", "-q", "-m", &subject, "-m", &line])?;
    Ok(sha)
}

#[cfg(test)]
mod tests {
    use super::web_url;

    #[test]
    fn turns_remote_urls_into_web_pages() {
        assert_eq!(
            web_url("git@github.com:cjpais/handy.git").as_deref(),
            Some("https://github.com/cjpais/handy")
        );
        assert_eq!(
            web_url("https://github.com/cjpais/handy.git").as_deref(),
            Some("https://github.com/cjpais/handy")
        );
        assert_eq!(web_url("C:/tmp/upstream"), None);
    }
}
