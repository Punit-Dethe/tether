//! Throwaway repositories for end-to-end tests.
//!
//! A [`Fixture`] is an upstream repo and a fork that share NO history, which
//! is the case tether exists for (a project downloaded, not forked). Each repo
//! sets its own identity so tests never read or touch the machine's git
//! config.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

pub struct Repo {
    pub path: PathBuf,
}

impl Repo {
    pub fn init(path: PathBuf) -> Self {
        fs::create_dir_all(&path).unwrap();
        let repo = Repo { path };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.name", "tether-test"]);
        repo.git(&["config", "user.email", "tether-test@example.invalid"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        repo.git(&["config", "core.autocrlf", "false"]);
        repo
    }

    pub fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    pub fn write(&self, file: &str, text: &str) {
        let path = self.path.join(file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    /// Stage everything and commit; returns the new commit's sha.
    pub fn commit(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }
}

pub struct Fixture {
    _tmp: TempDir,
    pub upstream: Repo,
    pub fork: Repo,
}

impl Fixture {
    /// An upstream with one commit, and a fork started from a copy of its
    /// files (not its history), with `upstream` configured as a remote.
    pub fn downloaded_fork() -> Self {
        let tmp = TempDir::new().unwrap();
        let upstream = Repo::init(tmp.path().join("upstream"));
        upstream.write("core/lib.rs", "pub fn answer() -> u32 { 42 }\n");
        upstream.commit("upstream: first release");

        let fork = Repo::init(tmp.path().join("fork"));
        fork.write("core/lib.rs", "pub fn answer() -> u32 { 42 }\n");
        fork.commit("fork: start from a downloaded copy");
        let url = upstream.path.to_string_lossy().replace('\\', "/");
        fork.git(&["remote", "add", "upstream", &url]);
        fork.git(&["fetch", "-q", "upstream"]);

        Fixture {
            _tmp: tmp,
            upstream,
            fork,
        }
    }

    pub fn empty_dir() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("plain");
        fs::create_dir_all(&path).unwrap();
        (tmp, path)
    }
}

/// What one tether run produced.
pub struct Run {
    pub code: i32,
    pub json: Value,
    pub stdout: String,
}

/// Run the built tether binary with `--json` in `dir`.
pub fn tether(dir: &Path, args: &[&str]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_tether"))
        .args(args)
        .arg("--json")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        // A temp dir can sit inside some other checkout; never let git
        // climb out of the fixture and find it.
        .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap_or(dir))
        .output()
        .expect("tether runs");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let json = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("not JSON ({error}):\n{stdout}"));
    Run {
        code: output.status.code().unwrap_or(-1),
        json,
        stdout,
    }
}
