//! Paths the way people actually have them: spaces, non-ASCII names, and
//! (on Windows) backslashes in `-C`. Reports always use upstream's
//! forward-slash paths, unquoted.

mod support;

use std::fs;

use support::{Repo, tether, tether_with};
use tempfile::TempDir;

const FILE: &str = "src dir/naïve file.rs";

#[test]
fn spaces_and_non_ascii_paths_round_trip() {
    let tmp = TempDir::new().unwrap();
    let outside = tmp.path().join("elsewhere");
    fs::create_dir_all(&outside).unwrap();

    let upstream = Repo::init(tmp.path().join("up stream ü"));
    upstream.write(FILE, "fn base() {}\n");
    let first = upstream.commit("first");
    upstream.write(FILE, "fn upstream() {}\n");
    upstream.commit("Change the naïve file (#5)");

    let fork = Repo::init(tmp.path().join("my fork é"));
    fork.write(FILE, "fn base() {}\n");
    fork.commit("start from a copy");
    let url = upstream.path.to_string_lossy().replace('\\', "/");
    fork.git(&["remote", "add", "upstream", &url]);
    fork.git(&["fetch", "-q", "upstream"]);
    let init = tether(&fork.path, &["init", "--sync-point", &first]);
    assert_eq!(init.code, 0, "{}", init.stdout);
    fork.write(FILE, "fn ours() {}\n");
    fork.commit("fork: ours");

    // `-C` with the platform's own separators, run from outside the repo.
    let repo = fork.path.to_string_lossy().into_owned();
    let plan = tether_with(&outside, &["-C", &repo, "plan"], &[]);
    assert_eq!(plan.json["ok"], true, "{}", plan.stdout);
    let tip = plan.json["data"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["kind"] == "tip")
        .unwrap();
    assert_eq!(tip["conflicts"][0], FILE, "{}", plan.stdout);
    assert_eq!(plan.json["data"]["commits"][0]["files"][0], FILE);

    let packet = tether_with(&outside, &["-C", &repo, "packet", "#5"], &[]);
    assert_eq!(packet.json["ok"], true, "{}", packet.stdout);
    assert!(
        packet.stdout.contains("naïve file.rs"),
        "packet names the file as written: {}",
        packet.stdout
    );

    let preview = tether_with(&outside, &["-C", &repo, "sync", "--dry-run"], &[]);
    assert!(
        preview.stdout.contains("src dir/naïve file.rs"),
        "{}",
        preview.stdout
    );
    assert!(!preview.stdout.contains("\\303"), "{}", preview.stdout);
}
