//! The only module that runs `git`.
//!
//! tether calls the real git binary on purpose: rename detection, rerere,
//! merge drivers and `replace --graft` only behave fully in git itself, and a
//! library reimplementation would differ silently. Keeping every invocation
//! here means there is exactly one place that decides how git is called.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};

/// Oldest git tether supports: `merge-tree --write-tree --merge-base` (2.40).
pub const MIN_VERSION: (u32, u32, u32) = (2, 40, 0);

/// Captured result of one git invocation. A non-zero exit is data, not an
/// error: many git questions (`merge-base --is-ancestor`) answer with it.
#[derive(Debug, Clone)]
pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn success(&self) -> bool {
        self.status == 0
    }
}

/// git, bound to one working directory.
#[derive(Debug, Clone)]
pub struct Git {
    dir: PathBuf,
}

impl Git {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Git { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A git command in this repository with stable, parseable output.
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        command
            // Paths as written (`naïve.rs`), not C-quoted (`"na\303\257ve.rs"`).
            .args(["-c", "core.quotepath=false"])
            .args(args)
            .current_dir(&self.dir)
            // The same output whatever the user's locale, and never block on
            // a credential prompt: an agent cannot answer it.
            .env("LC_ALL", "C")
            .env("GIT_TERMINAL_PROMPT", "0");
        command
    }

    /// Run git and capture its output, whatever the exit status.
    pub fn run(&self, args: &[&str]) -> Result<Output> {
        let output = self
            .command(args)
            .output()
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => Error::GitMissing,
                _ => Error::Io(error),
            })?;
        Ok(Output {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// Run git and return its trimmed stdout; a non-zero exit is an error.
    pub fn ok(&self, args: &[&str]) -> Result<String> {
        let output = self.run(args)?;
        if !output.success() {
            return Err(Error::GitFailed {
                args: args.join(" "),
                stderr: output.stderr.trim().to_owned(),
            });
        }
        Ok(output.stdout.trim().to_owned())
    }

    /// The installed git version, checked against [`MIN_VERSION`].
    pub fn version(&self) -> Result<(u32, u32, u32)> {
        let text = self.ok(&["--version"])?;
        let found = parse_version(&text).ok_or_else(|| Error::GitFailed {
            args: "--version".into(),
            stderr: format!("unrecognised version string: {text}"),
        })?;
        if found < MIN_VERSION {
            return Err(Error::GitTooOld {
                found: format!("{}.{}.{}", found.0, found.1, found.2),
                needed: format!("{}.{}.{}", MIN_VERSION.0, MIN_VERSION.1, MIN_VERSION.2),
            });
        }
        Ok(found)
    }

    /// The top of the working tree containing [`Git::dir`].
    pub fn toplevel(&self) -> Result<PathBuf> {
        let output = self.run(&["rev-parse", "--show-toplevel"])?;
        if !output.success() {
            return Err(Error::NotARepo(self.dir.clone()));
        }
        Ok(PathBuf::from(output.stdout.trim()))
    }

    /// Names of the configured remotes.
    pub fn remotes(&self) -> Result<Vec<String>> {
        Ok(self
            .ok(&["remote"])?
            .lines()
            .map(str::to_owned)
            .filter(|name| !name.is_empty())
            .collect())
    }

    /// Run git and return its raw stdout (untrimmed); a non-zero exit is an
    /// error. For output whose exact bytes matter, such as commit messages.
    pub fn raw(&self, args: &[&str]) -> Result<String> {
        let output = self.run(args)?;
        if !output.success() {
            return Err(Error::GitFailed {
                args: args.join(" "),
                stderr: output.stderr.trim().to_owned(),
            });
        }
        Ok(output.stdout)
    }

    /// The full sha of `rev` as a commit, or `None` if it names no commit.
    pub fn commit_sha(&self, rev: &str) -> Result<Option<String>> {
        let spec = format!("{rev}^{{commit}}");
        let output = self.run(&[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &spec,
        ])?;
        let sha = output.stdout.trim();
        Ok((output.success() && !sha.is_empty()).then(|| sha.to_owned()))
    }

    /// Is `older` an ancestor of (or equal to) `newer`?
    pub fn is_ancestor(&self, older: &str, newer: &str) -> Result<bool> {
        Ok(self
            .run(&["merge-base", "--is-ancestor", older, newer])?
            .success())
    }

    /// Does this clone hold `sha` as a commit object?
    pub fn has_commit(&self, sha: &str) -> Result<bool> {
        let spec = format!("{sha}^{{commit}}");
        Ok(self.run(&["cat-file", "-e", &spec])?.success())
    }

    /// Every commit reachable from `rev`.
    pub fn rev_list(&self, rev: &str) -> Result<Vec<String>> {
        Ok(self
            .raw(&["rev-list", rev])?
            .split_whitespace()
            .map(str::to_owned)
            .collect())
    }

    /// `(path, blob id)` for every file under `prefixes` in `rev`
    /// (all files when `prefixes` is empty).
    pub fn ls_tree(&self, rev: &str, prefixes: &[String]) -> Result<Vec<(String, String)>> {
        let mut args = vec!["ls-tree", "-r", "-z", "--full-tree", rev, "--"];
        // An empty prefix is the whole tree (a vendored upstream's folder
        // maps to upstream's root), and git refuses it as a pathspec.
        if !prefixes.iter().any(String::is_empty) {
            args.extend(prefixes.iter().map(String::as_str));
        }
        let out = self.raw(&args)?;
        Ok(out
            .split('\0')
            .filter_map(|entry| {
                let (meta, path) = entry.split_once('\t')?;
                let mut fields = meta.split(' ');
                let (_mode, kind, id) = (fields.next()?, fields.next()?, fields.next()?);
                (kind == "blob").then(|| (path.to_owned(), id.to_owned()))
            })
            .collect())
    }

    /// The contents of many blobs through one `git cat-file --batch`.
    pub fn blobs(&self, ids: &[&str]) -> Result<Vec<Vec<u8>>> {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::process::Stdio;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut child = self
            .command(&["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => Error::GitMissing,
                _ => Error::Io(error),
            })?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let request: String = ids.iter().map(|id| format!("{id}\n")).collect();
        // Write from another thread so a full stdout pipe cannot deadlock us.
        let writer = std::thread::spawn(move || stdin.write_all(request.as_bytes()));
        let mut reader = BufReader::new(child.stdout.take().expect("piped stdout"));
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let mut header = String::new();
            reader.read_line(&mut header)?;
            let size: usize = header
                .split_whitespace()
                .nth(2)
                .and_then(|size| size.parse().ok())
                .ok_or_else(|| Error::GitFailed {
                    args: format!("cat-file --batch ({id})"),
                    stderr: header.trim().to_owned(),
                })?;
            let mut body = vec![0; size + 1]; // contents, then a newline
            reader.read_exact(&mut body)?;
            body.pop();
            out.push(body);
        }
        writer.join().expect("writer thread")?;
        child.wait()?;
        Ok(out)
    }

    /// Run git and keep raw bytes: (exit status, stdout, stderr). For
    /// commands whose output is file content, not text.
    pub fn run_bytes(&self, args: &[&str]) -> Result<(i32, Vec<u8>, Vec<u8>)> {
        let output = self
            .command(args)
            .output()
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => Error::GitMissing,
                _ => Error::Io(error),
            })?;
        Ok((
            output.status.code().unwrap_or(-1),
            output.stdout,
            output.stderr,
        ))
    }

    /// Run git with `input` on stdin; a non-zero exit is an error.
    pub fn ok_with_input(&self, args: &[&str], input: &str) -> Result<String> {
        use std::io::Write;
        use std::process::Stdio;
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => Error::GitMissing,
                _ => Error::Io(error),
            })?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let request = input.to_owned();
        let writer = std::thread::spawn(move || stdin.write_all(request.as_bytes()));
        let output = child.wait_with_output()?;
        writer.join().expect("writer thread")?;
        if !output.status.success() {
            return Err(Error::GitFailed {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// Absolute path of a file inside the git directory (`SQUASH_MSG`, ...).
    /// Correct in linked worktrees, where `.git` is a file.
    pub fn git_path(&self, name: &str) -> Result<PathBuf> {
        let path = PathBuf::from(self.ok(&["rev-parse", "--git-path", name])?);
        Ok(if path.is_absolute() {
            path
        } else {
            self.dir.join(path)
        })
    }
}

/// Parse `git version 2.52.0.windows.1` (or `2.39.3 (Apple Git-146)`).
pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let raw = text.trim().strip_prefix("git version ")?;
    let mut parts = raw
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<u32>().ok());
    let major = parts.next()??;
    let minor = parts.next()??;
    let patch = parts.next().flatten().unwrap_or(0);
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::parse_version;

    #[test]
    fn parses_the_version_strings_git_actually_prints() {
        assert_eq!(
            parse_version("git version 2.52.0.windows.1"),
            Some((2, 52, 0))
        );
        assert_eq!(
            parse_version("git version 2.39.3 (Apple Git-146)"),
            Some((2, 39, 3))
        );
        assert_eq!(parse_version("git version 2.43.0\n"), Some((2, 43, 0)));
        assert_eq!(parse_version("git version 3.0"), Some((3, 0, 0)));
    }

    #[test]
    fn rejects_text_that_is_not_a_git_version() {
        assert_eq!(parse_version("hg version 6.0"), None);
        assert_eq!(parse_version("git version"), None);
    }
}
