//! `tether map`: the divergence map, generated from intent markers.

use std::path::Path;

use serde_json::json;

use super::Fork;
use crate::error::{Error, Result};
use crate::map;
use crate::report::{Next, NextKind, Report};
use crate::syncpoint;

pub struct Args<'a> {
    pub write: bool,
    pub reason: Option<&'a str>,
    pub why: Option<&'a str>,
}

pub const MAP_FILE: &str = "MAP.md";

pub fn run(dir: &Path, args: Args) -> Result<Report> {
    let fork = Fork::open(dir)?;
    if let Some(path) = args.reason {
        return record_reason(&fork, path, args.why.unwrap_or(""));
    }
    let base = syncpoint::sync_point(&fork.git, "HEAD")?.upstream;
    let files = map::scan(&fork.git, &fork.root, &fork.config, &base)?;
    let marker = &fork.config.map.marker;
    let text = map::render(&files, &base, marker);
    let unmarked: usize = files.iter().map(|f| f.unmarked.len()).sum();
    if args.write {
        let path = fork.root.join(crate::config::state_dir()).join(MAP_FILE);
        std::fs::create_dir_all(path.parent().expect("has a parent"))?;
        std::fs::write(path, &text)?;
    }
    let summary = if fork.config.map_scope().is_empty() {
        "No map scope: set [map] scope (or [budget] scope) to the folders that hold upstream-derived code.".to_owned()
    } else {
        format!(
            "{} diverged file(s), {} marker(s), {unmarked} unmarked change(s){}.",
            files.len(),
            files.iter().map(|f| f.markers.len()).sum::<usize>(),
            if args.write {
                "; wrote .tether/MAP.md"
            } else {
                ""
            }
        )
    };
    let report = Report::success(
        "map",
        summary,
        json!({ "sync_point": base, "marker": marker, "unmarked": unmarked, "files": files }),
    )
    .with_detail(text);
    Ok(
        match files
            .iter()
            .find_map(|f| f.unmarked.first().map(|c| (f, c)))
        {
            Some((file, change)) => {
                report.then(mark_next(&fork.config.map.marker, &file.path, change.line))
            }
            None => report,
        },
    )
}

/// Mark one change: a comment at the change, or a reason for the whole file.
pub(crate) fn mark_next(marker: &str, path: &str, line: usize) -> Next {
    Next::of(
        NextKind::Edit,
        "tether check",
        format!(
            "{path}:{line} changes upstream code with no `{marker}` comment saying why; add one on or just above the change (`{marker} <why>`), or, for a file that cannot hold comments, run `tether map --reason {path} --why \"<why>\"`"
        ),
    )
}

fn record_reason(fork: &Fork, path: &str, why: &str) -> Result<Report> {
    let path = path.replace('\\', "/");
    let mut reasons = map::load_reasons(&fork.root)?;
    let why = why.trim();
    if why.is_empty() {
        if reasons.remove(&path).is_none() {
            return Err(Error::Refused(format!(
                "say why with --why \"…\" (no reason is recorded for {path} to remove)"
            )));
        }
    } else {
        if !fork.root.join(&path).is_file() {
            return Err(Error::Refused(format!("{path} is not a file in this fork")));
        }
        reasons.insert(path.clone(), why.to_owned());
    }
    map::save_reasons(&fork.root, &reasons)?;
    Ok(Report::success(
        "map",
        if why.is_empty() {
            format!("Removed the recorded reason for {path}.")
        } else {
            format!("Recorded why {path} differs, in .tether/reasons.toml.")
        },
        json!({ "path": path, "reason": (!why.is_empty()).then_some(why) }),
    ))
}
