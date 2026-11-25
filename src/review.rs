//! Review gate: every upstream commit that touches a review area needs a
//! review record with evidence before the sync point may move past it.

use std::path::Path;

use serde::Serialize;

use crate::config::Config;
use crate::decisions::{self, Decisions};
use crate::engine::touched_in_range;
use crate::error::Result;
use crate::git::Git;

#[derive(Debug, Clone, Serialize)]
pub struct Missing {
    pub sha: String,
    pub subject: String,
    pub area: String,
    pub failures: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Audit {
    /// (commit, area) pairs that needed a review.
    pub required: usize,
    pub missing: Vec<Missing>,
}

/// Check every upstream commit in `base..head` that is in `upstream_ref`.
pub fn audit(
    git: &Git,
    root: &Path,
    config: &Config,
    decisions: &Decisions,
    base: &str,
    head: &str,
) -> Result<Audit> {
    let mut audit = Audit::default();
    if config.reviews.is_empty() {
        return Ok(audit);
    }
    let upstream_ref = config.upstream_ref();
    for (sha, paths) in touched_in_range(git, base, head)? {
        let mut areas: Vec<&str> = paths
            .iter()
            .flat_map(|path| config.review_areas_for(path))
            .collect();
        areas.sort_unstable();
        areas.dedup();
        if areas.is_empty() || !git.is_ancestor(&sha, &upstream_ref)? {
            continue;
        }
        for area in areas {
            audit.required += 1;
            let record = decisions.get(&sha).and_then(|d| d.review.get(area));
            let failures = decisions::review_failures(record, root);
            if !failures.is_empty() {
                let subject = git.ok(&["show", "-s", "--format=%s", &sha])?;
                audit.missing.push(Missing {
                    sha: sha.clone(),
                    subject,
                    area: area.to_owned(),
                    failures,
                });
            }
        }
    }
    Ok(audit)
}
