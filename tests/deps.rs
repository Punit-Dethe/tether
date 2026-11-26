//! `tether deps`: libraries taken from git. Your forks are checked against
//! the project they came from; libraries your upstream pins are tagged as
//! theirs, not hidden; floating pins are flagged.

mod support;

use std::path::Path;

use support::{Repo, tether};
use tempfile::TempDir;

fn url(repo: &Repo) -> String {
    repo.path.to_string_lossy().replace('\\', "/")
}

struct World {
    _tmp: TempDir,
    app: Repo,
    fork_head: String,
}

/// A library (`parent`), our fork of it with one patch, a library our
/// upstream pins, an unpinned one, and an app that uses all three.
fn world() -> World {
    let tmp = TempDir::new().unwrap();
    let at = |name: &str| tmp.path().join(name);

    let parent = Repo::init(at("parent"));
    parent.write("src/lib.rs", "pub fn parse() {}\n");
    parent.write("src/net.rs", "pub fn get() {}\n");
    parent.commit("first");
    parent.git(&["tag", "v1.0"]);

    let fork = Repo::init(at("fork"));
    fork.git(&["remote", "add", "origin", &url(&parent)]);
    fork.git(&["pull", "-q", "--no-rebase", "origin", "main"]);
    fork.write("src/lib.rs", "pub fn parse() { /* faster */ }\n");
    let fork_head = fork.commit("our patch: faster parse");

    // Upstream moves on: a fix near our patch, a security fix elsewhere,
    // something unrelated, and a release.
    parent.write("src/lib.rs", "pub fn parse() { /* fixed */ }\n");
    parent.commit("Fix parse on empty input");
    parent.write("src/net.rs", "pub fn get() { /* checked */ }\n");
    parent.commit("Fix CVE-2026-12345 header overflow");
    parent.write("README.md", "docs\n");
    parent.commit("Add docs");
    parent.git(&["tag", "v1.1"]);

    let theirs = Repo::init(at("theirs"));
    theirs.write("a.txt", "a\n");
    let theirs_head = theirs.commit("theirs");
    let loose = Repo::init(at("loose"));
    loose.write("a.txt", "a\n");
    loose.commit("loose");

    let app = Repo::init(at("app"));
    // What the upstream project builds: it pins `theirs`.
    app.write(
        "Cargo.toml",
        &format!(
            "[package]\nname = \"app\"\n\n[dependencies]\ntheirs = {{ git = \"{}\", rev = \"{theirs_head}\" }}\n",
            url(&theirs)
        ),
    );
    let upstream_tip = app.commit("upstream app");
    app.git(&["update-ref", "refs/remotes/upstream/main", &upstream_tip]);
    app.write(
        "Cargo.toml",
        &format!(
            "[package]\nname = \"app\"\n\n[dependencies]\ntheirs = {{ git = \"{}\", rev = \"{theirs_head}\" }}\nparser = {{ git = \"{}\", rev = \"{fork_head}\", package = \"parser\" }}\nloose = {{ git = \"{}\" }}\n",
            url(&theirs),
            url(&fork),
            url(&loose)
        ),
    );
    app.write(
        ".tether/config.toml",
        &format!(
            "[upstream]\nremote = \"upstream\"\nbranch = \"main\"\nname = \"Handy\"\n\n[[deps.fork]]\nurl = \"{}\"\nupstream = \"{}\"\n",
            url(&fork),
            url(&parent)
        ),
    );
    app.commit("our app");
    World {
        _tmp: tmp,
        app,
        fork_head,
    }
}

fn library<'a>(json: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    json["data"]["libraries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["name"] == name)
        .unwrap_or_else(|| panic!("no {name} in {json:#}"))
}

#[test]
fn sorts_libraries_by_whose_and_checks_your_forks() {
    let w = world();
    let run = tether(&w.app.path, &["deps"]);
    assert_eq!(run.code, 2, "a security fix is waiting: {}", run.stdout);
    let json = &run.json;
    assert_eq!(json["data"]["counts"]["yours"], 1, "{json:#}");
    assert_eq!(json["data"]["counts"]["upstream"], 1);
    assert_eq!(json["data"]["counts"]["other"], 1);

    let fork = library(json, "fork");
    assert_eq!(fork["whose"], "yours");
    let state = &fork["fork"];
    assert_eq!(state["pinned"], w.fork_head.as_str());
    assert_eq!(state["ahead"], 1);
    assert_eq!(state["behind"], 3);
    assert_eq!(state["your_files"], 1);
    assert_eq!(state["security"], 1);
    assert_eq!(state["near_yours"], 1);
    assert_eq!(state["releases"], serde_json::json!(["v1.1"]));
    assert_eq!(state["conflicts"], serde_json::json!(["src/lib.rs"]));
    // Only the commits that need a look, not "Add docs".
    let subjects: Vec<&str> = state["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["subject"].as_str().unwrap())
        .collect();
    assert_eq!(subjects.len(), 2, "{subjects:?}");
    assert!(!subjects.contains(&"Add docs"));

    let theirs = library(json, "theirs");
    assert_eq!(theirs["whose"], "upstream");
    assert_eq!(theirs["maintained_by"], "Handy");
    assert!(theirs.get("flags").is_none(), "same pin as upstream");

    let loose = library(json, "loose");
    assert_eq!(loose["whose"], "other");
    assert_eq!(loose["flags"], serde_json::json!(["unpinned"]));

    assert_eq!(json["next"]["command"], "tether deps fork");
}

#[test]
fn one_library_lists_every_upstream_change() {
    let w = world();
    let run = tether(&w.app.path, &["deps", "parser"]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    let libraries = run.json["data"]["libraries"].as_array().unwrap();
    assert_eq!(libraries.len(), 1);
    assert_eq!(libraries[0]["fork"]["changes"].as_array().unwrap().len(), 3);
    // One library also lists your fork's own commits and changed files.
    assert_eq!(
        libraries[0]["fork"]["patches"][0]["subject"],
        "our patch: faster parse"
    );
    assert_eq!(
        libraries[0]["fork"]["files"],
        serde_json::json!(["src/lib.rs"])
    );
    assert!(
        run.json["summary"]
            .as_str()
            .unwrap()
            .contains("3 commits behind"),
        "{}",
        run.json["summary"]
    );

    let missing = tether(&w.app.path, &["deps", "nothing"]);
    assert_eq!(missing.code, 1);
}

#[test]
fn works_offline_from_the_cache_and_without_tether_set_up() {
    let w = world();
    assert_eq!(tether(&w.app.path, &["deps"]).code, 2);
    let offline = tether(&w.app.path, &["deps", "--no-fetch"]);
    assert_eq!(offline.code, 2, "{}", offline.stdout);
    assert_eq!(library(&offline.json, "fork")["fork"]["behind"], 3);

    std::fs::remove_dir_all(w.app.path.join(".tether")).unwrap();
    let bare = tether(&w.app.path, &["deps", "--no-fetch"]);
    assert_eq!(bare.code, 0, "{}", bare.stdout);
    assert_eq!(bare.json["data"]["counts"]["other"], 3);
}

#[test]
fn a_project_without_git_dependencies_says_so() {
    let tmp = TempDir::new().unwrap();
    let app = Repo::init(tmp.path().join("app"));
    app.write("Cargo.toml", "[dependencies]\nserde = \"1\"\n");
    app.commit("app");
    let run = tether(Path::new(&app.path), &["deps"]);
    assert_eq!(run.code, 0);
    assert!(
        run.json["summary"]
            .as_str()
            .unwrap()
            .starts_with("No dependencies")
    );
}
