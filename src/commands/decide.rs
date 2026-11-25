//! `tether decide`: record what the sync point cannot infer about a commit.

use std::path::Path;

use serde_json::json;

use super::Fork;
use crate::decisions::{self, Port, PortOutcome, Review, ReviewOutcome, Status};
use crate::error::{Error, Result};
use crate::inbox::{self, Item, State};
use crate::report::Report;

pub struct Args<'a> {
    pub item: &'a str,
    pub outcome: Option<&'a str>,
    pub note: Option<&'a str>,
    pub port: Option<&'a str>,
    pub review: Option<&'a str>,
    pub problem: Option<&'a str>,
    pub evidence: Option<&'a str>,
    pub destinations: &'a [String],
    pub clear: bool,
}

pub fn run(dir: &Path, args: Args) -> Result<Report> {
    let fork = Fork::open(dir)?;
    let inbox = inbox::build(&fork.git, &fork.root, &fork.config)?;
    let item = resolve(&inbox.items, args.item)?;
    let all = decisions::load(&fork.root)?;
    let mut decision = all.get(&item.sha).cloned().unwrap_or_default();
    let refuse = |message: String| Err(Error::Refused(message));

    if args.clear {
        decision = Default::default();
    } else if let Some(source) = args.port {
        let Some(relocation) = fork.config.relocations.get(source) else {
            return refuse(format!("`{source}` is not a relocated file in the config"));
        };
        let outcome = match args.outcome {
            Some("ported") => PortOutcome::Ported,
            Some("not-applicable") => PortOutcome::NotApplicable,
            Some("deferred") => PortOutcome::Deferred,
            _ => return refuse("a port outcome is ported, not-applicable or deferred".into()),
        };
        let evidence = args.evidence.unwrap_or("").trim();
        if evidence.is_empty() {
            return refuse(
                "a port record needs --evidence: how you checked the fork's code".into(),
            );
        }
        decision.port.insert(
            source.to_owned(),
            Port {
                outcome,
                destinations: relocation.fork.clone(),
                evidence: evidence.to_owned(),
            },
        );
    } else if let Some(area) = args.review {
        if !fork.config.reviews.iter().any(|r| r.name == area) {
            return refuse(format!("`{area}` is not a review area in the config"));
        }
        let outcome = match args.outcome {
            Some("adapted") => ReviewOutcome::Adapted,
            Some("already-covered") => ReviewOutcome::AlreadyCovered,
            Some("not-applicable") => ReviewOutcome::NotApplicable,
            _ => {
                return refuse(
                    "a review outcome is adapted, already-covered or not-applicable".into(),
                );
            }
        };
        let review = Review {
            outcome,
            problem: args.problem.unwrap_or("").trim().to_owned(),
            destinations: args.destinations.to_vec(),
            evidence: args.evidence.unwrap_or("").trim().to_owned(),
        };
        let failures = decisions::review_failures(Some(&review), &fork.root);
        if !failures.is_empty() {
            return refuse(format!("review not recorded: {}", failures.join("; ")));
        }
        decision.review.insert(area.to_owned(), review);
    } else {
        let derived = if item.absorbed {
            State::Merged
        } else {
            State::Pending
        };
        match args.outcome {
            Some("merged") if derived == State::Merged => decision.status = None,
            Some("merged") => decision.status = Some(Status::Merged),
            Some("ignored") => decision.status = Some(Status::Ignored),
            Some("pending") if derived == State::Pending => decision.status = None,
            Some("pending") => {
                return refuse(
                    "the sync point already contains this commit; it cannot be pending".into(),
                );
            }
            Some(other) => return refuse(format!("unknown outcome `{other}`")),
            None if args.note.is_none() => {
                return refuse("say what was decided: an outcome, --note, or --clear".into());
            }
            None => {}
        }
    }
    if let Some(note) = args.note {
        let note = note.trim();
        decision.note = (!note.is_empty()).then(|| note.to_owned());
    }

    decisions::save(&fork.root, &item.sha, &decision)?;
    let file = format!(".tether/decisions/{}.toml", item.sha);
    let summary = if decision.is_empty() {
        format!(
            "{} now follows the sync point; no decision stored.",
            &item.sha[..8]
        )
    } else {
        format!("Recorded in {file}.")
    };
    Ok(Report::success(
        "decide",
        summary,
        json!({ "sha": item.sha, "subject": item.subject, "file": file, "decision": decision }),
    ))
}

/// A short sha or `#123` / `123` pull-request reference → one tracked commit.
fn resolve<'a>(items: &'a [Item], reference: &str) -> Result<&'a Item> {
    let reference = reference.trim();
    let pr = reference.strip_prefix('#').or_else(|| {
        (reference.len() <= 6 && reference.bytes().all(|b| b.is_ascii_digit())).then_some(reference)
    });
    let hits: Vec<&Item> = match pr {
        Some(pr) => items
            .iter()
            .filter(|i| i.pr.as_deref() == Some(pr))
            .collect(),
        None => {
            let prefix = reference.to_ascii_lowercase();
            items
                .iter()
                .filter(|i| i.sha.starts_with(&prefix))
                .collect()
        }
    };
    match hits[..] {
        [one] => Ok(one),
        [] => Err(Error::Refused(format!(
            "no tracked upstream commit matches `{reference}`"
        ))),
        _ => Err(Error::Refused(format!(
            "`{reference}` is ambiguous: {}",
            hits.iter()
                .map(|i| &i.sha[..10])
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}
