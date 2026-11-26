//! `tether inbox`: tracked upstream commits and their state.

use std::path::Path;

use serde_json::json;

use super::Fork;
use crate::error::Result;
use crate::inbox::{self, State};
use crate::report::Report;

pub fn run(dir: &Path, all: bool, security: bool) -> Result<Report> {
    let fork = Fork::open(dir)?;
    let inbox = inbox::build(&fork.git, &fork.root, &fork.config)?;
    let pending = inbox.count(State::Pending);
    let items: Vec<_> = inbox
        .items
        .iter()
        .filter(|item| {
            if security {
                !item.security.is_empty()
                    && (all || matches!(item.state, State::Pending | State::Unreleased))
            } else {
                all || item.state == State::Pending
            }
        })
        .collect();
    let flagged: Vec<&str> = inbox
        .items
        .iter()
        .filter(|i| !i.security.is_empty() && matches!(i.state, State::Pending | State::Unreleased))
        .map(|i| i.sha.as_str())
        .collect();
    let mut summary = format!(
        "{pending} pending, {} merged, {} ignored; {} upstream commit(s) past the sync point {}.",
        inbox.count(State::Merged),
        inbox.count(State::Ignored),
        inbox.behind,
        &inbox.sync_point.upstream[..8],
    );
    if let Some(release) = &inbox.release {
        summary.push_str(&format!(
            " Tracking release {release}; {} commit(s) unreleased.",
            inbox.unreleased
        ));
    }
    if !flagged.is_empty() {
        summary = format!(
            "{} possible security fix(es) not in the fork yet. {summary}",
            flagged.len()
        );
    }
    let data = json!({
        "sync_point": inbox.sync_point,
        "target": inbox.target,
        "release": inbox.release,
        "behind": inbox.behind,
        "unreleased": inbox.unreleased,
        "security": flagged,
        "counts": {
            "pending": pending,
            "merged": inbox.count(State::Merged),
            "ignored": inbox.count(State::Ignored),
        },
        "folders": crate::folders::load(&fork.root)?
            .folders
            .iter()
            .map(|folder| json!({
                "name": folder.name,
                "count": inbox.items.iter().filter(|i| i.folder.as_deref() == Some(folder.name.as_str())).count(),
            }))
            .collect::<Vec<_>>(),
        "items": items,
    });
    Ok(Report::success("inbox", summary, data))
}
