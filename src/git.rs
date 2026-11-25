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

    /// Run git and capture its output, whatever the exit status.
    pub fn run(&self, args: &[&str]) -> Result<Output> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.dir)
            // Stable, parseable output regardless of the user's locale, and
            // never block on a credential prompt: an agent cannot answer it.
            .env("LC_ALL", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
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
