//! Build/test gates: run the fork's own commands on three trees and compare.
//!
//! - **baseline** — the fork before the sync,
//! - **upstream** — upstream as shipped (the sync target),
//! - **result**   — the working tree being synced.
//!
//! A failure in the result is *new* only when neither the baseline nor
//! upstream has it; only new failures block a sync. That is what tells a
//! broken merge apart from upstream shipping something broken.
//!
//! Baseline and upstream run in throwaway `git worktree`s in the system temp
//! folder, removed as soon as their steps finish. Results and
//! logs stay in `.git/tether/verify/` (never committed), keyed by tree id so
//! an unchanged tree is not rebuilt.

use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::{Step, Verify};
use crate::error::{Error, Result};
use crate::git::Git;

/// How much of a failing step's log a record keeps inline.
const TAIL_BYTES: usize = 3_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Baseline,
    Upstream,
    Result,
}

impl Target {
    pub fn label(self) -> &'static str {
        match self {
            Target::Baseline => "baseline",
            Target::Upstream => "upstream",
            Target::Result => "result",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRun {
    pub name: String,
    /// Not run on this tree (`upstream = ""`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
    pub exit: Option<i32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
    pub seconds: f64,
    /// Failure ids, `<step>` for a step that failed as a whole or
    /// `<step>: <test>` for one test.
    pub failures: Vec<String>,
    pub log: String,
    /// The end of the log, when the step failed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tail: String,
}

/// One target's run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub target: Target,
    /// The commit checked out (none for the working tree).
    pub commit: Option<String>,
    /// The tree id the run covers: an unchanged key means a reusable run.
    pub key: String,
    pub at: u64,
    pub steps: Vec<StepRun>,
}

impl Run {
    pub fn failures(&self) -> BTreeSet<String> {
        self.steps
            .iter()
            .flat_map(|step| step.failures.iter().cloned())
            .collect()
    }

    /// Steps that ran on this tree.
    fn ran(&self) -> BTreeSet<&str> {
        self.steps
            .iter()
            .filter(|step| !step.skipped)
            .map(|step| step.name.as_str())
            .collect()
    }
}

/// The comparison of the three runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Verdict {
    /// The result tree this verdict covers.
    pub key: String,
    /// In the result only: the merge broke these. They block the sync.
    pub new: Vec<String>,
    /// Upstream ships these broken too.
    pub inherited: Vec<String>,
    /// Already broken in the fork before the sync.
    pub preexisting: Vec<String>,
    /// Broken before, passing now.
    pub fixed: Vec<String>,
    /// Steps upstream skipped, whose failures cannot be blamed on upstream.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unchecked_upstream: Vec<String>,
}

pub fn compare(result: &Run, baseline: Option<&Run>, upstream: Option<&Run>) -> Verdict {
    let empty = BTreeSet::new();
    let found = result.failures();
    let before = baseline.map_or(empty.clone(), Run::failures);
    let shipped = upstream.map_or(empty, Run::failures);
    let unchecked = match upstream {
        Some(up) => {
            let ran = up.ran();
            result
                .ran()
                .into_iter()
                .filter(|name| !ran.contains(name))
                .map(str::to_owned)
                .collect()
        }
        None => Vec::new(),
    };
    Verdict {
        key: result.key.clone(),
        new: found
            .iter()
            .filter(|f| !before.contains(*f) && !shipped.contains(*f))
            .cloned()
            .collect(),
        inherited: found.intersection(&shipped).cloned().collect(),
        preexisting: found
            .iter()
            .filter(|f| before.contains(*f) && !shipped.contains(*f))
            .cloned()
            .collect(),
        fixed: before.difference(&found).cloned().collect(),
        unchecked_upstream: unchecked,
    }
}

/// Failure ids a step's output names.
pub fn parse(step: &Step, output: &str) -> Vec<String> {
    let mut ids: Vec<String> = output
        .lines()
        .filter_map(|line| failure_in(step, line))
        .map(|id| format!("{}: {id}", step.name))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn failure_in(step: &Step, line: &str) -> Option<String> {
    let line = line.trim_end();
    if let Some(prefix) = &step.prefix {
        return line
            .strip_prefix(prefix.as_str())
            .map(|rest| rest.trim().to_owned());
    }
    let trimmed = line.trim_start();
    match step.format.as_str() {
        "cargo" => trimmed
            .strip_prefix("test ")?
            .strip_suffix(" ... FAILED")
            .map(str::to_owned),
        "pytest" => {
            let rest = trimmed.strip_prefix("FAILED ")?;
            Some(rest.split(" - ").next().unwrap_or(rest).trim().to_owned())
        }
        "go" => trimmed
            .strip_prefix("--- FAIL: ")?
            .split_whitespace()
            .next()
            .map(str::to_owned),
        "tap" => {
            let rest = trimmed.strip_prefix("not ok")?;
            let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit() || c == ' ');
            let rest = rest.strip_prefix("- ").unwrap_or(rest);
            let rest = rest.split(" # ").next().unwrap_or(rest);
            Some(rest.trim().to_owned())
        }
        _ => None,
    }
}

/// Where runs, logs and throwaway worktrees live.
pub fn dir(git: &Git) -> Result<PathBuf> {
    git.git_path("tether/verify")
}

fn record_path(git: &Git, name: &str) -> Result<PathBuf> {
    Ok(dir(git)?.join(format!("{name}.json")))
}

pub fn load_run(git: &Git, target: Target) -> Result<Option<Run>> {
    load(&record_path(git, target.label())?)
}

pub fn load_verdict(git: &Git) -> Result<Option<Verdict>> {
    load(&record_path(git, "verdict")?)
}

pub fn save_run(git: &Git, run: &Run) -> Result<()> {
    save(&record_path(git, run.target.label())?, run)
}

pub fn save_verdict(git: &Git, verdict: &Verdict) -> Result<()> {
    save(&record_path(git, "verdict")?, verdict)
}

fn load<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(serde_json::from_str(&text).ok()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(value).expect("records are serialisable");
    std::fs::write(path, text)?;
    Ok(())
}

/// The tree id of a commit.
pub fn commit_key(git: &Git, commit: &str) -> Result<String> {
    git.ok(&["rev-parse", &format!("{commit}^{{tree}}")])
}

/// The tree id of the working tree's tracked files, without touching the
/// index or the stash list (`git stash create` only writes objects).
pub fn working_key(git: &Git) -> Result<String> {
    let stash = git.ok(&["stash", "create"])?;
    let commit = if stash.is_empty() {
        "HEAD"
    } else {
        stash.as_str()
    };
    commit_key(git, commit)
}

/// Run `steps` in `dir`, logging to `.git/tether/verify/<target>-<step>.log`.
pub fn execute(
    git: &Git,
    settings: &Verify,
    target: Target,
    workdir: &Path,
) -> Result<Vec<StepRun>> {
    let logs = dir(git)?;
    std::fs::create_dir_all(&logs)?;
    let timeout = Duration::from_secs(settings.timeout_secs.unwrap_or(3_600));
    let mut runs = Vec::new();
    for step in &settings.steps {
        let line = match (target, &step.upstream) {
            (Target::Upstream, Some(line)) => line.as_str(),
            _ => step.run.as_str(),
        };
        let safe: String = step
            .name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let log = logs.join(format!("{}-{safe}.log", target.label()));
        let log_text = log.display().to_string();
        if line.trim().is_empty() {
            runs.push(StepRun {
                name: step.name.clone(),
                skipped: true,
                exit: None,
                timed_out: false,
                seconds: 0.0,
                failures: Vec::new(),
                log: log_text,
                tail: String::new(),
            });
            continue;
        }
        let started = Instant::now();
        let (exit, timed_out) = spawn_logged(settings, line, workdir, &log, timeout)?;
        let output = String::from_utf8_lossy(&std::fs::read(&log)?).into_owned();
        let mut failures = parse(step, &output);
        let failed = timed_out || exit != Some(0);
        if failed && failures.is_empty() {
            failures.push(step.name.clone());
        }
        let tail = if failures.is_empty() {
            String::new()
        } else {
            let mut cut = output.len().saturating_sub(TAIL_BYTES);
            while !output.is_char_boundary(cut) {
                cut += 1;
            }
            output[cut..].to_owned()
        };
        runs.push(StepRun {
            name: step.name.clone(),
            skipped: false,
            exit,
            timed_out,
            seconds: (started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
            failures,
            log: log_text,
            tail,
        });
    }
    Ok(runs)
}

/// Run one command line with stdout and stderr into `log`. Returns the exit
/// code (`None` when it was killed or could not start) and whether it timed
/// out.
fn spawn_logged(
    settings: &Verify,
    line: &str,
    workdir: &Path,
    log: &Path,
    timeout: Duration,
) -> Result<(Option<i32>, bool)> {
    let out = File::create(log)?;
    let err = out.try_clone()?;
    let mut command = shell_command(&settings.shell, line);
    command
        .current_dir(workdir)
        .envs(&settings.env)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            std::fs::write(log, format!("could not start `{line}`: {error}\n"))?;
            return Ok((None, false));
        }
    };
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status.code(), false));
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Ok((None, true));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn shell_command(shell: &[String], line: &str) -> Command {
    if let [program, args @ ..] = shell {
        let mut command = Command::new(program);
        command.args(args).arg(line);
        return command;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("cmd");
        // cmd parses its own command line; pass it through untouched.
        command.arg("/C").raw_arg(line);
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("sh");
        command.arg("-c").arg(line);
        command
    }
}

/// Check `commit` out into a throwaway worktree, run `f` there, and remove
/// the worktree whatever happens.
pub fn in_worktree<T>(
    git: &Git,
    target: Target,
    commit: &str,
    f: impl FnOnce(&Path) -> Result<T>,
) -> Result<T> {
    // Outside the repository: build tools that walk up from a checkout
    // (Cargo workspaces, npm) must not find the fork around it.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&dir(git)?, &mut hasher);
    let id = std::hash::Hasher::finish(&hasher);
    let path = std::env::temp_dir().join(format!("tether-{id:x}-{}", target.label()));
    let path_text = path.to_string_lossy().into_owned();
    remove_worktree(git, &path_text);
    git.ok(&[
        "worktree", "add", "--detach", "--force", "--quiet", &path_text, commit,
    ])?;
    let outcome = f(&path);
    remove_worktree(git, &path_text);
    outcome
}

fn remove_worktree(git: &Git, path: &str) {
    let _ = git.run(&["worktree", "remove", "--force", path]);
    let _ = std::fs::remove_dir_all(path);
    let _ = git.run(&["worktree", "prune"]);
}

/// Refuse a commit that is not in this clone.
pub fn require_commit(git: &Git, commit: &str) -> Result<()> {
    if git.has_commit(commit)? {
        Ok(())
    } else {
        Err(Error::Refused(format!("{commit} is not in this clone")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(format: &str, prefix: Option<&str>) -> Step {
        Step {
            name: "t".into(),
            run: String::new(),
            format: format.into(),
            prefix: prefix.map(str::to_owned),
            upstream: None,
        }
    }

    #[test]
    fn formats_name_each_failure() {
        let cargo = "test a::b ... ok\ntest a::c ... FAILED\n";
        assert_eq!(parse(&step("cargo", None), cargo), vec!["t: a::c"]);
        let pytest = "FAILED tests/x.py::test_y - AssertionError\n";
        assert_eq!(
            parse(&step("pytest", None), pytest),
            vec!["t: tests/x.py::test_y"]
        );
        let go = "--- FAIL: TestZ (0.00s)\n";
        assert_eq!(parse(&step("go", None), go), vec!["t: TestZ"]);
        let tap = "ok 1 - fine\nnot ok 2 - broken # TODO\n";
        assert_eq!(parse(&step("tap", None), tap), vec!["t: broken"]);
        let custom = "FAIL alpha\nFAIL alpha\nok beta\n";
        assert_eq!(
            parse(&step("exit", Some("FAIL ")), custom),
            vec!["t: alpha"]
        );
    }

    fn run(target: Target, failures: &[&str]) -> Run {
        Run {
            target,
            commit: None,
            key: target.label().into(),
            at: 0,
            steps: vec![StepRun {
                name: "t".into(),
                skipped: false,
                exit: Some(0),
                timed_out: false,
                seconds: 0.0,
                failures: failures.iter().map(|f| (*f).to_owned()).collect(),
                log: String::new(),
                tail: String::new(),
            }],
        }
    }

    #[test]
    fn only_failures_in_neither_input_are_new() {
        let base = run(Target::Baseline, &["old", "gone"]);
        let up = run(Target::Upstream, &["shipped"]);
        let result = run(Target::Result, &["old", "shipped", "mine"]);
        let verdict = compare(&result, Some(&base), Some(&up));
        assert_eq!(verdict.new, vec!["mine"]);
        assert_eq!(verdict.inherited, vec!["shipped"]);
        assert_eq!(verdict.preexisting, vec!["old"]);
        assert_eq!(verdict.fixed, vec!["gone"]);
    }
}
