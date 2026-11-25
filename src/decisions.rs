//! Recorded decisions about upstream commits: `.tether/decisions/<sha>.toml`.
//!
//! One file per commit, so two people (or agents) deciding different commits
//! never touch the same file. A decision only says what the sync point cannot
//! infer: that a commit was ignored, or applied by hand, why, and with what
//! evidence a port or review was checked.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::STATE_DIR;
use crate::error::{Error, Result};

pub const DIR: &str = "decisions";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Its work is in the fork although the sync point has not passed it.
    Merged,
    /// Deliberately not taken.
    Ignored,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PortOutcome {
    Ported,
    NotApplicable,
    Deferred,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewOutcome {
    Adapted,
    AlreadyCovered,
    NotApplicable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Port {
    pub outcome: PortOutcome,
    #[serde(default)]
    pub destinations: Vec<String>,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub outcome: ReviewOutcome,
    /// What broke upstream, in one or two sentences.
    pub problem: String,
    #[serde(default)]
    pub destinations: Vec<String>,
    /// How the decision was checked.
    pub evidence: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Per relocated upstream file: did its fix reach the fork's code?
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub port: BTreeMap<String, Port>,
    /// Per review area: what the upstream change meant for the fork.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub review: BTreeMap<String, Review>,
}

impl Decision {
    pub fn is_empty(&self) -> bool {
        *self == Decision::default()
    }
}

pub type Decisions = BTreeMap<String, Decision>;

pub fn dir(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join(DIR)
}

/// Every recorded decision, keyed by full upstream sha.
pub fn load(root: &Path) -> Result<Decisions> {
    let mut all = Decisions::new();
    let entries = match std::fs::read_dir(dir(root)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(all),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let path = entry?.path();
        let Some(sha) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".toml"))
        else {
            continue;
        };
        let bad = |message: String| Error::BadConfig {
            path: format!("{STATE_DIR}/{DIR}/{sha}.toml"),
            message,
        };
        if sha.len() != 40 || !sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(bad("file name must be a full lowercase sha".into()));
        }
        let text = std::fs::read_to_string(&path)?;
        let decision = toml::from_str(&text).map_err(|error| bad(error.to_string()))?;
        all.insert(sha.to_owned(), decision);
    }
    Ok(all)
}

pub fn save(root: &Path, sha: &str, decision: &Decision) -> Result<()> {
    let path = dir(root).join(format!("{sha}.toml"));
    if decision.is_empty() {
        match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => return Ok(()),
        }
    }
    std::fs::create_dir_all(dir(root))?;
    let text = toml::to_string(decision).expect("a decision is always serialisable");
    std::fs::write(path, text)?;
    Ok(())
}

/// Why a review record does not yet count; empty when it does.
pub fn review_failures(review: Option<&Review>, root: &Path) -> Vec<String> {
    let Some(review) = review else {
        return vec!["review record is missing".into()];
    };
    let mut failures = Vec::new();
    if review.problem.trim().is_empty() {
        failures.push("underlying problem is missing".into());
    }
    if review.evidence.trim().is_empty() {
        failures.push("verification evidence is missing".into());
    }
    let needs_destination = matches!(
        review.outcome,
        ReviewOutcome::Adapted | ReviewOutcome::AlreadyCovered
    );
    if needs_destination && review.destinations.is_empty() {
        failures.push("this outcome requires at least one fork destination".into());
    }
    failures.extend(destination_failures(&review.destinations, root));
    failures
}

/// Destinations must be existing, repository-relative paths.
pub fn destination_failures(destinations: &[String], root: &Path) -> Vec<String> {
    let mut failures = Vec::new();
    for destination in destinations {
        let trimmed = destination.trim();
        let path = Path::new(trimmed);
        if trimmed.is_empty() {
            failures.push("destination is empty".into());
        } else if path.is_absolute() || path.has_root() {
            failures.push(format!(
                "destination must be repository-relative: {destination}"
            ));
        } else if path.components().any(|c| matches!(c, Component::ParentDir)) {
            failures.push(format!("destination escapes the repository: {destination}"));
        } else if !root.join(path).exists() {
            failures.push(format!("destination does not exist: {destination}"));
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_decision_round_trips_through_toml() {
        let mut decision = Decision {
            status: Some(Status::Ignored),
            note: Some("replaced by our own settings".into()),
            ..Decision::default()
        };
        decision.port.insert(
            "src/settings.rs".into(),
            Port {
                outcome: PortOutcome::NotApplicable,
                destinations: vec!["src/ours.rs".into()],
                evidence: "read both".into(),
            },
        );
        let text = toml::to_string(&decision).unwrap();
        assert_eq!(toml::from_str::<Decision>(&text).unwrap(), decision);
    }

    #[test]
    fn reviews_need_problem_evidence_and_real_destinations() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let review = Review {
            outcome: ReviewOutcome::Adapted,
            problem: " ".into(),
            destinations: vec!["../x".into(), "Cargo.toml".into(), "nope.rs".into()],
            evidence: "tests".into(),
        };
        let failures = review_failures(Some(&review), root);
        assert_eq!(failures.len(), 3, "{failures:?}");
        assert!(review_failures(None, root)[0].contains("missing"));
    }
}
