//! The agent contract, end to end: every command answers with the same JSON
//! envelope and a meaningful exit code.

mod support;

use serde_json::Value;
use support::{Fixture, tether};

fn assert_envelope(json: &Value, command: &str) {
    assert_eq!(json["command"], command);
    assert_eq!(json["tether"], env!("CARGO_PKG_VERSION"));
    for key in ["ok", "summary", "data", "next", "error"] {
        assert!(
            json.get(key).is_some(),
            "envelope is missing `{key}`: {json}"
        );
    }
}

#[test]
fn doctor_passes_on_a_downloaded_fork_with_an_upstream_remote() {
    let fx = Fixture::downloaded_fork();
    let run = tether(&fx.fork.path, &["doctor"]);
    assert_envelope(&run.json, "doctor");
    assert_eq!(run.code, 0, "{}", run.stdout);
    assert_eq!(run.json["ok"], true);
    assert_eq!(run.json["next"], Value::Null);
    let checks = run.json["data"]["checks"].as_array().unwrap();
    assert!(checks.iter().all(|c| c["ok"] == true), "{}", run.stdout);
}

#[test]
fn doctor_says_how_to_add_a_missing_upstream_remote() {
    let fx = Fixture::downloaded_fork();
    fx.fork.git(&["remote", "remove", "upstream"]);
    let run = tether(&fx.fork.path, &["doctor"]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    assert_eq!(run.json["ok"], true);
    let next = run.json["next"]["command"].as_str().unwrap();
    assert!(next.starts_with("git remote add upstream"), "{next}");
}

#[test]
fn doctor_explains_a_directory_that_is_not_a_repository() {
    let (_tmp, dir) = Fixture::empty_dir();
    let run = tether(&dir, &["doctor"]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    let checks = run.json["data"]["checks"].as_array().unwrap();
    let repo = checks.iter().find(|c| c["name"] == "repository").unwrap();
    assert_eq!(repo["ok"], false);
}

#[test]
fn status_points_an_unconfigured_fork_at_init() {
    let fx = Fixture::downloaded_fork();
    let run = tether(&fx.fork.path, &["status"]);
    assert_envelope(&run.json, "status");
    assert_eq!(run.code, 2, "{}", run.stdout);
    assert_eq!(run.json["data"]["configured"], false);
    assert_eq!(run.json["next"]["command"], "tether init");
}

#[test]
fn status_outside_a_repository_is_a_coded_error() {
    let (_tmp, dir) = Fixture::empty_dir();
    let run = tether(&dir, &["status"]);
    assert_eq!(run.code, 1);
    assert_eq!(run.json["ok"], false);
    assert_eq!(run.json["error"]["code"], "not_a_repo");
}

#[test]
fn unbuilt_commands_say_which_milestone_brings_them() {
    let fx = Fixture::downloaded_fork();
    for args in [vec!["verify"], vec!["packet", "abc123"], vec!["map"]] {
        let run = tether(&fx.fork.path, &args);
        assert_envelope(&run.json, args[0]);
        assert_eq!(run.code, 1, "{}", run.stdout);
        assert_eq!(run.json["error"]["code"], "not_implemented");
    }
}

#[test]
fn the_fixture_shares_no_history_with_upstream() {
    let fx = Fixture::downloaded_fork();
    let output = std::process::Command::new("git")
        .args(["merge-base", "HEAD", "upstream/main"])
        .current_dir(&fx.fork.path)
        .output()
        .unwrap();
    assert!(!output.status.success(), "fixture repos must be unrelated");
}
