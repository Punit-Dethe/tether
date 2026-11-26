//! `tether plan`: the furthest conflict-free step and the easiest-first order.

mod support;

use serde_json::Value;
use support::{Fixture, tether};

#[test]
fn plan_stops_before_the_first_conflict_and_ranks_commits() {
    let fx = Fixture::downloaded_fork();
    let first = fx.upstream.git(&["rev-parse", "HEAD"]);
    fx.upstream.write("docs/guide.md", "hello\n");
    let docs = fx.upstream.commit("Add a guide (#1)");
    fx.upstream
        .write("core/lib.rs", "pub fn answer() -> u32 { 43 }\n");
    let fix = fx.upstream.commit("Fix the answer (#2)");
    fx.fork.git(&["fetch", "-q", "upstream"]);
    let init = tether(&fx.fork.path, &["init", "--sync-point", &first]);
    assert_eq!(init.code, 0, "{}", init.stdout);
    // The fork changed the same line, so the fix conflicts.
    fx.fork
        .write("core/lib.rs", "pub fn answer() -> u32 { 41 }\n");
    fx.fork.commit("fork: our answer");

    let run = tether(&fx.fork.path, &["plan"]);
    let report: Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(report["ok"], true, "{}", run.stdout);
    let steps = report["data"]["steps"].as_array().unwrap();
    let clean = steps
        .iter()
        .find(|s| s["kind"] == "clean")
        .expect("a clean step");
    assert_eq!(clean["commit"], docs.as_str());
    assert_eq!(clean["commits"], 1);
    assert_eq!(clean["conflicts"].as_array().unwrap().len(), 0);
    let tip = steps.iter().find(|s| s["kind"] == "tip").unwrap();
    assert_eq!(tip["conflicts"][0], "core/lib.rs");
    assert!(
        report["next"]["command"]
            .as_str()
            .unwrap()
            .starts_with("tether sync --target ")
    );

    let commits = report["data"]["commits"].as_array().unwrap();
    assert_eq!(commits[0]["sha"], docs.as_str());
    assert_eq!(commits[0]["ease"], "clean");
    assert_eq!(commits[1]["sha"], fix.as_str());
    assert_eq!(commits[1]["ease"], "conflict");
    assert_eq!(commits[1]["files"][0], "core/lib.rs");

    // Planning changed nothing.
    assert_eq!(fx.fork.git(&["status", "--porcelain"]), "");
}

#[test]
fn folders_file_commits_and_show_in_the_inbox() {
    let fx = Fixture::downloaded_fork();
    let first = fx.upstream.git(&["rev-parse", "HEAD"]);
    fx.upstream.write("docs/guide.md", "hello\n");
    fx.upstream.commit("Add a guide (#1)");
    fx.fork.git(&["fetch", "-q", "upstream"]);
    assert_eq!(
        tether(&fx.fork.path, &["init", "--sync-point", &first]).code,
        0
    );

    let filed = tether(&fx.fork.path, &["folder", "Docs", "#1"]);
    assert_eq!(filed.code, 0, "{}", filed.stdout);
    let inbox: Value =
        serde_json::from_str(&tether(&fx.fork.path, &["inbox"]).stdout).unwrap();
    assert_eq!(inbox["data"]["items"][0]["folder"], "Docs");
    let list: Value =
        serde_json::from_str(&tether(&fx.fork.path, &["folder"]).stdout).unwrap();
    assert_eq!(list["data"]["folders"][0]["count"], 1);
    assert_eq!(tether(&fx.fork.path, &["folder", "--unfile", "#1"]).code, 0);
    let inbox: Value =
        serde_json::from_str(&tether(&fx.fork.path, &["inbox"]).stdout).unwrap();
    assert!(inbox["data"]["items"][0].get("folder").is_none());
}
