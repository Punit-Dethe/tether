//! `tether deps`: libraries taken from git, whose they are, and what changed
//! upstream of your own forks.

use std::path::Path;

use serde_json::json;

use crate::config::Config;
use crate::deps::{self, Library, Whose};
use crate::error::{Error, Result};
use crate::git::Git;
use crate::report::{Next, Report};

pub fn run(dir: &Path, name: Option<&str>, no_fetch: bool) -> Result<Report> {
    let root = Git::new(dir).toplevel()?;
    let git = Git::new(&root);
    // Works without tether set up; the config only adds "maintained by
    // your upstream" and explicit fork parents.
    let config = if Config::exists(&root) {
        Some(Config::load(&root)?)
    } else {
        None
    };
    let libraries = deps::libraries(
        &git,
        &root,
        config.as_ref(),
        &deps::Options {
            fetch: !no_fetch,
            only: name,
        },
    )?;
    if let Some(name) = name {
        if libraries.is_empty() {
            return Err(Error::Refused(format!(
                "no library from git is called `{name}`; run `tether deps` to list them"
            )));
        }
    }

    let count = |whose: Whose| libraries.iter().filter(|l| l.whose == whose).count();
    let (yours, upstream, other) = (
        count(Whose::Yours),
        count(Whose::Upstream),
        count(Whose::Other),
    );
    let upstream_name = config
        .as_ref()
        .map_or("your upstream", |c| c.upstream_name())
        .to_owned();
    let security: Vec<&Library> = libraries
        .iter()
        .filter(|l| l.fork.as_ref().is_some_and(|f| f.security > 0))
        .collect();

    let summary = if libraries.is_empty() {
        "No dependencies come from git; nothing to track.".to_owned()
    } else if let Some(name) = name {
        one_line(&libraries[0], name)
    } else {
        let mut parts = vec![format!("{yours} yours")];
        if upstream > 0 {
            parts.push(format!("{upstream} maintained by {upstream_name}"));
        }
        if other > 0 {
            parts.push(format!("{other} someone else's"));
        }
        let mut text = format!(
            "{} {} from git: {}.",
            libraries.len(),
            plural(libraries.len(), "library", "libraries"),
            parts.join(", ")
        );
        if !security.is_empty() {
            let names: Vec<&str> = security.iter().map(|l| l.name.as_str()).collect();
            text.push_str(&format!(
                " Upstream has security fixes for {}.",
                names.join(", ")
            ));
        }
        text
    };

    let data = json!({
        "libraries": libraries,
        "counts": { "yours": yours, "upstream": upstream, "other": other },
        "upstream_name": upstream_name,
    });
    let mut report =
        Report::success("deps", summary, data).with_detail(table(&libraries, name.is_some()));
    if let Some(first) = security.first() {
        if name.is_none() {
            report = report.then(Next::run(
                format!("tether deps {}", first.name),
                "see the upstream security fix your fork does not have",
            ));
        }
        report = report.needs_action();
    } else if name.is_none() {
        if let Some(behind) = libraries
            .iter()
            .filter(|l| l.fork.as_ref().is_some_and(|f| f.near_yours > 0))
            .max_by_key(|l| l.fork.as_ref().map_or(0, |f| f.near_yours))
        {
            report = report.then(Next::run(
                format!("tether deps {}", behind.name),
                "upstream changed files your fork also changes",
            ));
        }
    }
    Ok(report)
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

fn one_line(library: &Library, name: &str) -> String {
    match (&library.fork, &library.problem) {
        (Some(fork), _) if fork.behind == 0 => {
            format!("{name} is up to date with {}.", short_repo(&fork.upstream))
        }
        (Some(fork), _) => format!(
            "{name} is {} {} behind {}{}.",
            fork.behind,
            plural(fork.behind, "commit", "commits"),
            short_repo(&fork.upstream),
            match (fork.security, fork.near_yours) {
                (0, 0) => String::new(),
                (s, 0) => format!("; {s} look like security fixes"),
                (0, n) => format!("; {n} touch files your fork changed"),
                (s, n) =>
                    format!("; {s} look like security fixes, {n} touch files your fork changed"),
            }
        ),
        (None, Some(problem)) => format!("{name}: {problem}"),
        (None, None) => match &library.maintained_by {
            Some(by) => format!("{name} is maintained by {by}."),
            None => format!("{name} is someone else's library."),
        },
    }
}

/// `owner/repo` from a repository address.
fn short_repo(url: &str) -> String {
    let key = deps::normalize(url);
    match deps::parts(&key) {
        Some((_, owner, repo)) => {
            // Keep the case the address was written in.
            let written = url.trim_end_matches('/').trim_end_matches(".git");
            let tail = &written[written.len().saturating_sub(owner.len() + repo.len() + 1)..];
            if tail.eq_ignore_ascii_case(&format!("{owner}/{repo}")) {
                tail.to_owned()
            } else {
                format!("{owner}/{repo}")
            }
        }
        None => url.to_owned(),
    }
}

/// The text a person reads: one block per library, grouped by whose.
fn table(libraries: &[Library], full: bool) -> String {
    let mut out = String::new();
    let mut last: Option<Whose> = None;
    for library in libraries {
        if last != Some(library.whose) {
            if last.is_some() {
                out.push('\n');
            }
            out.push_str(match library.whose {
                Whose::Yours => "Yours\n",
                Whose::Upstream => "Maintained upstream\n",
                Whose::Other => "Someone else's\n",
            });
            last = Some(library.whose);
        }
        let pins: Vec<String> = {
            let mut pins: Vec<String> = library.uses.iter().map(|u| u.pin.text()).collect();
            pins.sort();
            pins.dedup();
            pins
        };
        let mut line = format!(
            "  {:<16} {} @ {}",
            library.name,
            short_repo(&library.url),
            pins.join(" / ")
        );
        if let Some(by) = &library.maintained_by {
            line.push_str(&format!("  [{by}]"));
        }
        out.push_str(&line);
        out.push('\n');

        let mut notes: Vec<String> = Vec::new();
        if let Some(fork) = &library.fork {
            notes.push(format!(
                "{} ahead, {} behind {} ({}), {} {} changed",
                fork.ahead,
                fork.behind,
                short_repo(&fork.upstream),
                fork.upstream_branch,
                fork.your_files,
                plural(fork.your_files, "file", "files"),
            ));
            if fork.security > 0 {
                notes.push(format!(
                    "{} upstream {} look like security fixes",
                    fork.security,
                    plural(fork.security, "commit", "commits")
                ));
            }
            if fork.near_yours > 0 {
                notes.push(format!(
                    "{} upstream {} touch files your fork changed",
                    fork.near_yours,
                    plural(fork.near_yours, "commit", "commits")
                ));
            }
            if !fork.releases.is_empty() {
                notes.push(format!("not in your pin: {}", fork.releases.join(", ")));
            }
            match &fork.conflicts {
                Some(files) if files.is_empty() && fork.behind > 0 => {
                    notes.push("merging upstream would not conflict".into())
                }
                Some(files) if !files.is_empty() => {
                    let shown = if full { files.len() } else { 4 };
                    let mut text = format!(
                        "merging upstream would conflict in {} {}: {}",
                        files.len(),
                        plural(files.len(), "file", "files"),
                        files[..shown.min(files.len())].join(", ")
                    );
                    if files.len() > shown {
                        text.push_str(&format!(", … {} more", files.len() - shown));
                    }
                    notes.push(text)
                }
                _ => {}
            }
            if let Some(newer) = &fork.newer_on {
                notes.push(format!(
                    "your fork's {} has {} {} past the pin",
                    newer.branch,
                    newer.commits,
                    plural(newer.commits, "commit", "commits")
                ));
            }
        }
        if let Some(problem) = &library.problem {
            notes.push(problem.clone());
        }
        for flag in &library.flags {
            notes.push(match flag.as_str() {
                "unpinned" => {
                    "no pin: builds whatever the default branch is when the lockfile refreshes"
                        .into()
                }
                "floating" => "follows a branch, not a fixed commit".into(),
                "pinned_differently" => "pinned differently in different manifests".into(),
                "differs_from_upstream" => format!(
                    "upstream pins {}",
                    library.upstream_pin.as_deref().unwrap_or("it differently")
                ),
                other => other.to_owned(),
            });
        }
        if let Some(tip) = &library.tip {
            if tip.moved {
                let reference = match tip.reference.as_str() {
                    "HEAD" => "the default branch".to_owned(),
                    other => other
                        .trim_start_matches("refs/heads/")
                        .trim_start_matches("refs/tags/")
                        .to_owned(),
                };
                notes.push(format!(
                    "{reference} has moved to {} since your lockfile",
                    &tip.tip[..8.min(tip.tip.len())]
                ));
            }
        }
        for note in notes {
            out.push_str(&format!("      {note}\n"));
        }
        if let Some(fork) = &library.fork {
            let shown = if full { fork.changes.len() } else { 5 };
            for change in fork.changes.iter().take(shown) {
                let mark = if !change.security.is_empty() {
                    "!"
                } else if !change.near_yours.is_empty() {
                    "~"
                } else {
                    " "
                };
                let mut text = format!("    {mark} {} {}", &change.sha[..8], change.subject);
                if !change.security.is_empty() {
                    text.push_str(&format!("  ({})", change.security.join(", ")));
                }
                if !change.near_yours.is_empty() {
                    text.push_str(&format!("  [{}]", change.near_yours.join(", ")));
                }
                out.push_str(&text);
                out.push('\n');
            }
            if fork.changes.len() > shown {
                out.push_str(&format!(
                    "      … {} more: tether deps {}\n",
                    fork.changes.len() - shown,
                    library.name
                ));
            }
        }
        if full {
            for using in &library.uses {
                out.push_str(&format!(
                    "      used by {} [{}] as {}\n",
                    using.manifest, using.section, using.package
                ));
            }
        }
    }
    out
}
