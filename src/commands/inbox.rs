//! `tether inbox`: tracked upstream commits and their state.

use std::path::Path;

use serde_json::json;

use super::Fork;
use crate::error::Result;
use crate::inbox::{self, State};
use crate::report::Report;

pub fn run(dir: &Path, all: bool) -> Result<Report> {
    let fork = Fork::open(dir)?;
    let inbox = inbox::build(&fork.git, &fork.root, &fork.config)?;
    let pending = inbox.count(State::Pending);
    let items: Vec<_> = inbox
        .items
        .iter()
        .filter(|item| all || item.state == State::Pending)
        .collect();
    let summary = format!(
        "{pending} pending, {} merged, {} ignored; {} upstream commit(s) past the sync point {}.",
        inbox.count(State::Merged),
        inbox.count(State::Ignored),
        inbox.behind,
        &inbox.sync_point.upstream[..8],
    );
    let data = json!({
        "sync_point": inbox.sync_point,
        "target": inbox.target,
        "behind": inbox.behind,
        "counts": {
            "pending": pending,
            "merged": inbox.count(State::Merged),
            "ignored": inbox.count(State::Ignored),
        },
        "items": items,
    });
    Ok(Report::success("inbox", summary, data))
}
