//! `tether doctor`: can tether work here, and if not, what to fix.

use std::path::Path;

use serde_json::json;

use crate::error::{Error, Result};
use crate::git::Git;
use crate::report::Report;

/// One line of `tether doctor`.
fn check(name: &str, ok: bool, detail: impl Into<String>) -> serde_json::Value {
    json!({ "name": name, "ok": ok, "detail": detail.into() })
}

/// Can tether work here? Checks git, the repository and an upstream remote,
/// and says in plain words what to fix.
pub fn run(dir: &Path) -> Result<Report> {
    let git = Git::new(dir);
    let mut checks = Vec::new();
    let mut fix: Option<(String, String)> = None;

    match git.version() {
        Ok((major, minor, patch)) => {
            checks.push(check("git", true, format!("git {major}.{minor}.{patch}")));
        }
        Err(error @ (Error::GitMissing | Error::GitTooOld { .. })) => {
            checks.push(check("git", false, error.to_string()));
            let report = Report::success("doctor", error.to_string(), json!({ "checks": checks }))
                .with_next("install git 2.40 or newer", "every tether command runs git")
                .needs_action();
            return Ok(report);
        }
        Err(error) => return Err(error),
    }

    let root = match git.toplevel() {
        Ok(root) => {
            checks.push(check("repository", true, root.display().to_string()));
            root
        }
        Err(Error::NotARepo(path)) => {
            let detail = format!("{} is not inside a git repository", path.display());
            checks.push(check("repository", false, detail.clone()));
            let report = Report::success("doctor", detail, json!({ "checks": checks }))
                .with_next("cd into your fork", "tether works on one fork at a time")
                .needs_action();
            return Ok(report);
        }
        Err(error) => return Err(error),
    };

    let remotes = Git::new(&root).remotes()?;
    let upstream = remotes.iter().find(|name| name.as_str() == "upstream");
    match upstream {
        Some(name) => checks.push(check("upstream remote", true, name.clone())),
        None => {
            checks.push(check(
                "upstream remote",
                false,
                format!("no remote named `upstream` (found: {})", list(&remotes)),
            ));
            fix = Some((
                "git remote add upstream <url-of-the-project-you-forked>".into(),
                "tether compares your fork against this remote".into(),
            ));
        }
    }

    let failed = checks.iter().filter(|c| c["ok"] == false).count();
    let summary = if failed == 0 {
        "Everything tether needs is in place.".to_owned()
    } else {
        format!("{failed} check(s) need attention.")
    };
    let mut report = Report::success("doctor", summary, json!({ "checks": checks }));
    if let Some((command, why)) = fix {
        report = report.with_next(command, why).needs_action();
    }
    Ok(report)
}

fn list(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}
