//! M4's bar: an unmarked edit to upstream-derived code fails `check`.

mod support;

use std::fs;

use support::{Fixture, Repo, tether};

const LIB: &str = "fn one() {}\n\nfn two() {}\n\nfn three() {}\n\nfn four() {}\n\nfn five() {}\n";

struct World {
    _fx: Fixture,
    fork: Repo,
}

/// A fork whose `core/` came from upstream and is in scope, with one marked
/// change already in it.
fn world() -> World {
    let fx = Fixture::downloaded_fork();
    let root = fx.upstream.path.parent().unwrap().to_path_buf();
    let upstream = Repo::init(root.join("up-map"));
    upstream.write("core/lib.rs", LIB);
    upstream.write("core/data.json", "{\"a\": 1}\n");
    let base = upstream.commit("upstream: initial");

    let fork = Repo::init(root.join("fork-map"));
    fork.write(
        "core/lib.rs",
        &LIB.replace(
            "fn one() {}",
            "// tether: the fork's hook\nfn one() { hook(); }",
        ),
    );
    fork.write("core/data.json", "{\"a\": 1}\n");
    fork.write("feature.rs", "fn hook() {}\n");
    fork.commit(&format!("Initial commit\n\nUpstream-Synced: {base}"));
    let url = upstream.path.to_string_lossy().replace('\\', "/");
    fork.git(&["remote", "add", "upstream", &url]);
    fork.git(&["fetch", "-q", "upstream"]);
    fs::create_dir_all(fork.path.join(".tether")).unwrap();
    fork.write(
        ".tether/config.toml",
        "[upstream]\nname = \"Up\"\n\n[budget]\nscope = [\"core/\"]\n",
    );
    fork.commit("add tether config");
    let update = tether(&fork.path, &["check", "--update", "--accept-growth"]);
    assert_eq!(update.code, 0, "{}", update.stdout);
    fork.git(&["add", ".tether"]);
    fork.git(&["commit", "-q", "-m", "baseline"]);
    World { _fx: fx, fork }
}

#[test]
fn a_marked_change_passes_and_is_on_the_map() {
    let w = world();
    let check = tether(&w.fork.path, &["check"]);
    assert_eq!(check.code, 0, "{}", check.stdout);
    let map = tether(&w.fork.path, &["map", "--write"]);
    assert_eq!(map.code, 0, "{}", map.stdout);
    let file = &map.json["data"]["files"][0];
    assert_eq!(file["path"], "core/lib.rs");
    assert_eq!(file["markers"][0]["reason"], "the fork's hook");
    let written = fs::read_to_string(w.fork.path.join(".tether/MAP.md")).unwrap();
    assert!(written.contains("- L1: the fork's hook"), "{written}");
}

#[test]
fn an_unmarked_edit_fails_check_until_it_says_why() {
    let w = world();
    let lib = fs::read_to_string(w.fork.path.join("core/lib.rs")).unwrap();
    // "The setting changed for no reason": an edit with no marker.
    w.fork.write(
        "core/lib.rs",
        &lib.replace("fn five() {}", "fn five() { 9 }"),
    );
    w.fork.git(&["commit", "-qam", "tweak five"]);

    let check = tether(&w.fork.path, &["check"]);
    assert_eq!(check.code, 2, "{}", check.stdout);
    assert_eq!(
        check.json["data"]["unmarked"]["at"][0]["path"],
        "core/lib.rs"
    );
    assert_eq!(check.json["data"]["unmarked"]["at"][0]["line"], 10);
    assert_eq!(check.json["next"]["kind"], "edit");
    // An agent may not paper over it by accepting the growth.
    let sneak = support::tether_with(
        &w.fork.path,
        &["check", "--update", "--accept-growth"],
        &[("TETHER_ACTOR", "agent")],
    );
    assert_eq!(
        sneak.json["error"]["code"], "needs_human",
        "{}",
        sneak.stdout
    );

    // Say why, and it passes again.
    let lib = fs::read_to_string(w.fork.path.join("core/lib.rs")).unwrap();
    w.fork.write(
        "core/lib.rs",
        &lib.replace(
            "fn five() { 9 }",
            "// tether: five returns the fork's default\nfn five() { 9 }",
        ),
    );
    w.fork.git(&["commit", "-qam", "say why"]);
    // The marker problem is gone; the file's budget grew, which stays a
    // person's call, separately.
    let check = tether(&w.fork.path, &["check"]);
    assert_eq!(
        check.json["data"]["unmarked"]["failures"],
        serde_json::json!([])
    );
    let grew = check.json["data"]["budgets"]["failures"].to_string();
    assert!(grew.contains("GREW: core/lib.rs"), "{grew}");
    assert_eq!(check.json["next"]["kind"], "ask_human");
}

#[test]
fn a_file_without_comments_takes_a_recorded_reason() {
    let w = world();
    w.fork.write("core/data.json", "{\"a\": 2}\n");
    w.fork.git(&["commit", "-qam", "change data"]);
    let check = tether(&w.fork.path, &["check"]);
    assert_eq!(check.code, 2, "{}", check.stdout);
    assert_eq!(
        check.json["data"]["unmarked"]["at"][0]["path"],
        "core/data.json"
    );

    let reason = tether(
        &w.fork.path,
        &[
            "map",
            "--reason",
            "core/data.json",
            "--why",
            "the fork ships a=2",
        ],
    );
    assert_eq!(reason.code, 0, "{}", reason.stdout);
    let check = tether(&w.fork.path, &["check"]);
    assert_eq!(
        check.json["data"]["unmarked"]["failures"],
        serde_json::json!([])
    );
    let map = tether(&w.fork.path, &["map"]);
    let data = map.json["data"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "core/data.json")
        .unwrap()
        .clone();
    assert_eq!(data["reason"], "the fork ships a=2");
}

#[test]
fn fork_only_files_need_no_markers() {
    let w = world();
    w.fork
        .write("feature.rs", "fn hook() { lots(); of(); new(); code(); }\n");
    w.fork.write("core/extra.rs", "fn fork_only() {}\n");
    w.fork.git(&["add", "-A"]);
    w.fork.git(&["commit", "-qm", "feature work"]);
    let check = tether(&w.fork.path, &["check"]);
    assert_eq!(check.code, 0, "{}", check.stdout);
}
