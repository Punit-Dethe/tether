//! Proof that upstream's changes reached the fork, read from the code itself.
//!
//! Every upstream commit in a range is split into its hunks, and each hunk is
//! tested with `git apply --check` against private copies of two trees, so
//! nothing in the repository changes:
//!
//! 1. **Superseded**: the hunk no longer reverse-applies to upstream's own
//!    target. A later upstream commit rewrote those lines, so there is
//!    nothing of it left to find.
//! 2. **Landed**: it reverse-applies to the fork. Its result, with its
//!    context, is in the fork's code.
//! 3. **Missing**: it still applies forwards to the fork. The code is as it
//!    was before upstream's change.
//! 4. **Changed**: neither. The fork has something else there. tether
//!    cannot tell whether that is the change adapted or the change lost, so
//!    it is never counted as proven; it needs a recorded decision.
//!
//! Hunks in owned areas, and in relocated files the port audit covers, are
//! the fork's policy rather than a loss. A commit is proven only when every
//! one of its hunks is landed, superseded or covered by policy; a missing
//! or changed hunk needs a recorded decision that explains it.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::config::Config;
use crate::decisions::{Decision, Decisions, Status};
use crate::error::Result;
use crate::git::Git;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Every hunk that still matters is in the fork.
    Landed,
    /// Later upstream commits replaced all of it.
    Superseded,
    /// Its hunks fall in owned areas or relocated files: the fork's policy.
    Policy,
    /// None of it is in the fork.
    Missing,
    /// Some hunks landed, some are missing.
    Partial,
    /// The fork has different code where it changed things: not provable.
    Changed,
}

/// What the verdict means once recorded decisions are taken into account.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Finding {
    /// The code shows it: nothing to do.
    Proven,
    /// Not (fully) in the code, and a recorded decision says why.
    Explained,
    /// Missing from the code with no decision: an upstream change was lost.
    Dropped,
    /// A decision says it is in the fork; the code shows it is not.
    Contradicted,
    /// The fork's code differs and no decision explains how.
    Unproven,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FileProof {
    /// The path in upstream's tree.
    pub path: String,
    /// Where the fork keeps it, when that differs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork_path: Option<String>,
    pub landed: usize,
    pub missing: usize,
    pub superseded: usize,
    pub changed: usize,
    /// Hunks the fork's policy covers (owned area, relocated file).
    pub policy: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommitProof {
    pub sha: String,
    pub subject: String,
    pub verdict: Verdict,
    pub finding: Finding,
    /// The recorded decision that explains a gap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explained_by: Option<String>,
    pub files: Vec<FileProof>,
}

/// One file's part of a commit's diff.
#[derive(Debug, Default)]
struct FilePatch {
    old: Option<String>,
    new: Option<String>,
    /// Mode lines (`new file mode …`) kept for `git apply`.
    modes: Vec<String>,
    hunks: Vec<String>,
    binary: bool,
    /// Blob ids before and after (`index` line; full with `--full-index`).
    blobs: Option<(String, String)>,
}

impl FilePatch {
    fn path(&self) -> &str {
        self.new.as_deref().or(self.old.as_deref()).unwrap_or("")
    }

    /// A patch of the given hunks, with paths rewritten by `map`.
    fn patch(&self, hunks: &[usize], map: &dyn Fn(&str) -> String) -> String {
        let old = self.old.as_deref().map(map);
        let new = self.new.as_deref().map(map);
        let a = old.clone().or(new.clone()).unwrap_or_default();
        let b = new.clone().or(old.clone()).unwrap_or_default();
        let mut text = format!("diff --git a/{a} b/{b}\n");
        for mode in &self.modes {
            text.push_str(mode);
            text.push('\n');
        }
        text.push_str(&match &old {
            Some(old) => format!("--- a/{old}\n"),
            None => "--- /dev/null\n".into(),
        });
        text.push_str(&match &new {
            Some(new) => format!("+++ b/{new}\n"),
            None => "+++ /dev/null\n".into(),
        });
        for &index in hunks {
            text.push_str(&self.hunks[index]);
        }
        text
    }
}

/// Split `git diff` output into files and hunks.
fn parse(diff: &str) -> Vec<FilePatch> {
    let mut files: Vec<FilePatch> = Vec::new();
    let mut in_hunk = false;
    for line in diff.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if let Some(header) = bare.strip_prefix("diff --git ") {
            // Paths from the header, for patches with no ---/+++ lines
            // (binary files, mode changes). Without renames both are equal.
            let path = header
                .strip_prefix("a/")
                .and_then(|rest| rest.split_once(" b/"))
                .map(|(a, _)| a.to_owned());
            files.push(FilePatch {
                old: path.clone(),
                new: path,
                ..FilePatch::default()
            });
            in_hunk = false;
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if bare.starts_with("@@") {
            file.hunks.push(line.to_owned());
            in_hunk = true;
        } else if in_hunk {
            file.hunks.last_mut().expect("in a hunk").push_str(line);
        } else if let Some(path) = bare.strip_prefix("--- ") {
            file.old = strip_side(path, "a/");
        } else if let Some(path) = bare.strip_prefix("+++ ") {
            file.new = strip_side(path, "b/");
        } else if let Some(ids) = bare.strip_prefix("index ") {
            let ids = ids.split(' ').next().unwrap_or_default();
            if let Some((before, after)) = ids.split_once("..") {
                file.blobs = Some((before.to_owned(), after.to_owned()));
            }
        } else if bare.starts_with("new file mode")
            || bare.starts_with("deleted file mode")
            || bare.starts_with("old mode")
            || bare.starts_with("new mode")
        {
            if bare.starts_with("new file mode") {
                file.old = None;
            } else if bare.starts_with("deleted file mode") {
                file.new = None;
            }
            file.modes.push(bare.to_owned());
        } else if bare.starts_with("Binary files") || bare.starts_with("GIT binary patch") {
            file.binary = true;
        }
    }
    files
}

fn strip_side(path: &str, side: &str) -> Option<String> {
    let path = path.trim_end_matches('\t');
    (path != "/dev/null").then(|| path.strip_prefix(side).unwrap_or(path).to_owned())
}

/// Checks hunks against one tree through a private index.
struct Tree {
    git: Git,
    /// Path to blob id.
    files: HashMap<String, String>,
}

impl Tree {
    fn new(git: &Git, index: PathBuf, tree: &str) -> Result<Tree> {
        let git = git.with_index(index);
        git.ok(&["read-tree", tree])?;
        let files = git
            .raw(&["ls-tree", "-r", "-z", "--full-tree", tree])?
            .split('\0')
            .filter_map(|entry| {
                // "<mode> <type> <id>\t<path>"
                let (meta, path) = entry.split_once('\t')?;
                let id = meta.rsplit(' ').next()?;
                Some((path.to_owned(), id.to_owned()))
            })
            .collect();
        Ok(Tree { git, files })
    }

    fn applies(&self, patch: &str, reverse: bool) -> Result<bool> {
        let mut args = vec!["apply", "--cached", "--check"];
        if reverse {
            args.push("-R");
        }
        args.push("-");
        self.git.succeeds_with_input(&args, patch.as_bytes())
    }
}

/// Prove every non-merge upstream commit in `from..to` against the fork's
/// tree `fork_tree`, then judge each against the recorded decisions.
pub fn prove(
    git: &Git,
    config: &Config,
    decisions: &Decisions,
    from: &str,
    to: &str,
    fork_tree: &str,
) -> Result<Vec<CommitProof>> {
    let scratch = git.git_path("tether-prove")?;
    std::fs::create_dir_all(&scratch)?;
    // Private indexes are deleted however the proof ends.
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(scratch.clone());
    let fork = Tree::new(git, scratch.join("fork.index"), fork_tree)?;
    let upstream = Tree::new(git, scratch.join("upstream.index"), to)?;

    let range = format!("{from}..{to}");
    let commits = git.raw(&["rev-list", "--reverse", "--no-merges", &range])?;
    let mut out = Vec::new();
    for sha in commits.split_whitespace() {
        let subject = git.ok(&["log", "-1", "--format=%s", sha])?;
        let diff = git.raw(&[
            "diff",
            "--no-renames",
            "--no-color",
            "--no-ext-diff",
            "--full-index",
            "-U3",
            &format!("{sha}^"),
            sha,
        ])?;
        let files = parse(&diff)
            .iter()
            .filter(|file| !is_state(config, file.path()))
            .map(|file| prove_file(config, &fork, &upstream, file))
            .collect::<Result<Vec<_>>>()?;
        let verdict = verdict(&files);
        let (finding, explained_by) = judge(verdict, decisions.get(sha));
        out.push(CommitProof {
            sha: sha.to_owned(),
            subject,
            verdict,
            finding,
            explained_by,
            files,
        });
    }
    Ok(out)
}

fn is_state(config: &Config, path: &str) -> bool {
    config
        .state_dirs()
        .iter()
        .any(|dir| path.starts_with(dir.as_str()))
}

fn prove_file(
    config: &Config,
    fork: &Tree,
    upstream: &Tree,
    file: &FilePatch,
) -> Result<FileProof> {
    let path = file.path().to_owned();
    let fork_path = config.to_fork_path(&path);
    let mut proof = FileProof {
        fork_path: (fork_path != path).then(|| fork_path.clone()),
        path: path.clone(),
        ..FileProof::default()
    };
    if file.binary {
        prove_binary(config, fork, upstream, file, &fork_path, &mut proof);
        return Ok(proof);
    }
    if file.hunks.is_empty() {
        return Ok(proof); // a mode change only
    }
    let same = |p: &str| p.to_owned();
    let to_fork = |p: &str| config.to_fork_path(p);
    let all: Vec<usize> = (0..file.hunks.len()).collect();

    // 1. Hunks a later upstream commit rewrote.
    let live: Vec<usize> = if upstream.applies(&file.patch(&all, &same), true)? {
        all
    } else {
        let mut live = Vec::new();
        for &hunk in &all {
            if upstream.applies(&file.patch(&[hunk], &same), true)? {
                live.push(hunk);
            } else {
                proof.superseded += 1;
            }
        }
        live
    };
    if live.is_empty() {
        return Ok(proof);
    }

    // 2. Is the file in the fork at all?
    let owned = config.is_owned(&fork_path);
    let modified = file.old.is_some() && file.new.is_some();
    if modified && !fork.files.contains_key(&fork_path) {
        if owned {
            proof.policy = live.len();
            proof.why = Some("owned area");
        } else if config.relocations.contains_key(&path) {
            proof.policy = live.len();
            proof.why = Some("relocated: the port audit covers it");
        } else {
            proof.missing = live.len();
            proof.why = Some("not in the fork");
        }
        return Ok(proof);
    }

    // 3. Landed, missing or changed, hunk by hunk.
    if fork.applies(&file.patch(&live, &to_fork), true)? {
        proof.landed = live.len();
        return Ok(proof);
    }
    // Where the fork moved code out of this file, a hunk may be there.
    let moved_to: &[String] = config
        .relocations
        .get(&path)
        .map(|r| r.fork.as_slice())
        .unwrap_or_default();
    for &hunk in &live {
        let patch = file.patch(&[hunk], &to_fork);
        if fork.applies(&patch, true)? {
            proof.landed += 1;
        } else if owned {
            proof.policy += 1;
            proof.why = Some("owned area");
        } else if landed_elsewhere(fork, file, hunk, moved_to)? {
            proof.landed += 1;
            proof.why = Some("partly found in a relocated file");
        } else if !moved_to.is_empty() {
            // Code the fork relocated may live there reshaped: not provable
            // either way, so it is never called dropped.
            proof.changed += 1;
            proof.why = Some("relocated: check the files it moved to");
        } else if fork.applies(&patch, false)? {
            proof.missing += 1;
        } else {
            proof.changed += 1;
        }
    }
    Ok(proof)
}

/// Does the hunk's result sit, with its context, in one of `files`?
fn landed_elsewhere(fork: &Tree, file: &FilePatch, hunk: usize, files: &[String]) -> Result<bool> {
    if file.old.is_none() || file.new.is_none() {
        return Ok(false);
    }
    for target in files.iter().filter(|f| fork.files.contains_key(f.as_str())) {
        let patch = file.patch(&[hunk], &|_| target.clone());
        if fork.applies(&patch, true)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A binary file has no hunks: compare whole blobs instead.
fn prove_binary(
    config: &Config,
    fork: &Tree,
    upstream: &Tree,
    file: &FilePatch,
    fork_path: &str,
    proof: &mut FileProof,
) {
    let path = file.path();
    let Some((before, after)) = &file.blobs else {
        proof.changed = 1;
        proof.why = Some("binary: no blob ids");
        return;
    };
    let gone = |id: &str| id.bytes().all(|b| b == b'0');
    let (before, after) = (
        (!gone(before)).then_some(before.as_str()),
        (!gone(after)).then_some(after.as_str()),
    );
    // The file as the commit left it, against the fork and upstream's target.
    let in_upstream = upstream.files.get(path).map(String::as_str);
    let in_fork = fork.files.get(fork_path).map(String::as_str);
    if in_upstream != after {
        proof.superseded = 1;
    } else if in_fork == after {
        proof.landed = 1;
    } else if config.is_owned(fork_path) {
        proof.policy = 1;
        proof.why = Some("owned area");
    } else if in_fork == before {
        proof.missing = 1;
    } else {
        proof.changed = 1;
        proof.why = Some("binary");
    }
}

fn verdict(files: &[FileProof]) -> Verdict {
    let sum = |f: fn(&FileProof) -> usize| files.iter().map(f).sum::<usize>();
    let (landed, missing, changed, policy) = (
        sum(|f| f.landed),
        sum(|f| f.missing),
        sum(|f| f.changed),
        sum(|f| f.policy),
    );
    match (landed, missing, changed) {
        (_, 0, 0) if landed > 0 => Verdict::Landed,
        (0, 0, 0) if policy > 0 => Verdict::Policy,
        (0, 0, 0) => Verdict::Superseded,
        (0, _, 0) => Verdict::Missing,
        (_, m, _) if m > 0 => Verdict::Partial,
        _ => Verdict::Changed,
    }
}

/// The verdict against what was recorded about the commit.
fn judge(verdict: Verdict, decision: Option<&Decision>) -> (Finding, Option<String>) {
    let explanation = decision.and_then(explain);
    match verdict {
        Verdict::Landed | Verdict::Superseded | Verdict::Policy => (Finding::Proven, None),
        Verdict::Missing | Verdict::Partial
            if decision.is_some_and(|d| d.status == Some(Status::Merged)) =>
        {
            (Finding::Contradicted, Some("recorded as merged".into()))
        }
        _ => match explanation {
            Some(why) => (Finding::Explained, Some(why)),
            None if verdict == Verdict::Changed => (Finding::Unproven, None),
            None => (Finding::Dropped, None),
        },
    }
}

/// What in a decision accounts for upstream's change not being in the code.
fn explain(decision: &Decision) -> Option<String> {
    if decision.status == Some(Status::Ignored) {
        return Some(match &decision.note {
            Some(note) => format!("ignored: {note}"),
            None => "ignored".into(),
        });
    }
    if let Some((area, review)) = decision.review.iter().next() {
        return Some(format!("review {area}: {}", review.evidence));
    }
    if let Some((file, port)) = decision.port.iter().next() {
        return Some(format!("port {file}: {}", port.evidence));
    }
    decision.note.as_ref().map(|note| format!("note: {note}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs
index 111..222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,3 @@
 one
-two
+TWO
 three
@@ -10,3 +10,3 @@ fn x
 ten
-eleven
+ELEVEN
 twelve
diff --git a/new.txt b/new.txt
new file mode 100644
index 000..333
--- /dev/null
+++ b/new.txt
@@ -0,0 +1 @@
+hello
diff --git a/img.png b/img.png
index 1..2 100644
Binary files a/img.png and b/img.png differ
";

    #[test]
    fn splits_a_diff_into_files_and_hunks() {
        let files = parse(DIFF);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path(), "src/a.rs");
        assert_eq!(files[0].hunks.len(), 2);
        assert!(files[0].hunks[1].starts_with("@@ -10,3"));
        assert_eq!(files[1].old, None);
        assert_eq!(files[1].modes, vec!["new file mode 100644"]);
        assert!(files[2].binary);
        // No ---/+++ lines: the path and blob ids come from the header.
        assert_eq!(files[2].path(), "img.png");
        assert_eq!(files[2].blobs, Some(("1".into(), "2".into())));
    }

    #[test]
    fn one_hunk_patches_carry_mapped_paths() {
        let files = parse(DIFF);
        let patch = files[0].patch(&[1], &|p| format!("vendor/{p}"));
        assert!(patch.starts_with("diff --git a/vendor/src/a.rs b/vendor/src/a.rs\n--- a/vendor/src/a.rs\n+++ b/vendor/src/a.rs\n@@ -10,3"));
        assert!(!patch.contains("TWO"));
        let new = files[1].patch(&[0], &|p| p.to_owned());
        assert!(new.contains("new file mode 100644\n--- /dev/null\n+++ b/new.txt\n"));
    }

    #[test]
    fn verdicts_need_every_hunk() {
        let file = |landed, missing, changed| FileProof {
            landed,
            missing,
            changed,
            ..FileProof::default()
        };
        assert_eq!(verdict(&[file(2, 0, 0)]), Verdict::Landed);
        assert_eq!(verdict(&[file(0, 1, 0)]), Verdict::Missing);
        assert_eq!(verdict(&[file(1, 1, 0)]), Verdict::Partial);
        assert_eq!(verdict(&[file(1, 0, 1)]), Verdict::Changed);
        assert_eq!(verdict(&[file(0, 0, 0)]), Verdict::Superseded);
    }

    #[test]
    fn decisions_explain_gaps_but_never_overrule_the_code() {
        let merged = Decision {
            status: Some(Status::Merged),
            ..Decision::default()
        };
        let ignored = Decision {
            status: Some(Status::Ignored),
            ..Decision::default()
        };
        assert_eq!(judge(Verdict::Missing, None).0, Finding::Dropped);
        assert_eq!(
            judge(Verdict::Missing, Some(&merged)).0,
            Finding::Contradicted
        );
        assert_eq!(
            judge(Verdict::Missing, Some(&ignored)).0,
            Finding::Explained
        );
        assert_eq!(judge(Verdict::Changed, None).0, Finding::Unproven);
        assert_eq!(judge(Verdict::Landed, Some(&ignored)).0, Finding::Proven);
    }
}
