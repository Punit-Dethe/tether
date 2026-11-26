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

/// A long history where the step just before the first tip conflict still
/// conflicts (a file upstream changed and later brought back in line), so
/// the clean point has to be searched for.
#[test]
fn plan_searches_back_over_a_long_history() {
    let fx = Fixture::downloaded_fork();
    fx.upstream.write("a.txt", "base a\n");
    fx.upstream.write("b.txt", "base b\n");
    let first = fx.upstream.commit("add a and b");
    fx.fork.write("a.txt", "base a\n");
    fx.fork.write("b.txt", "base b\n");
    fx.fork.commit("fork: copy a and b");

    let filler = |from: usize, to: usize| -> Vec<String> {
        (from..to)
            .map(|n| {
                fx.upstream
                    .write(&format!("docs/{n}.md"), &format!("{n}\n"));
                fx.upstream.commit(&format!("Doc {n}"))
            })
            .collect()
    };
    let early = filler(0, 40);
    fx.upstream.write("a.txt", "upstream a\n");
    let diverge = fx.upstream.commit("Change a (#10)");
    filler(40, 80);
    fx.upstream.write("b.txt", "upstream b\n");
    let conflict = fx.upstream.commit("Change b (#11)");
    fx.upstream.write("a.txt", "fork a\n");
    fx.upstream.commit("Take the fork's a (#12)");
    filler(80, 100);

    fx.fork.git(&["fetch", "-q", "upstream"]);
    let init = tether(&fx.fork.path, &["init", "--sync-point", &first]);
    assert_eq!(init.code, 0, "{}", init.stdout);
    fx.fork.write("a.txt", "fork a\n");
    fx.fork.write("b.txt", "fork b\n");
    fx.fork.commit("fork: our a and b");

    let run = tether(&fx.fork.path, &["plan"]);
    let report = &run.json;
    assert_eq!(report["ok"], true, "{}", run.stdout);
    assert_eq!(report["data"]["behind"], 103);
    let steps = report["data"]["steps"].as_array().unwrap();
    let clean = steps.iter().find(|s| s["kind"] == "clean").unwrap();
    assert_eq!(clean["commit"], early[39].as_str(), "{}", run.stdout);
    assert_eq!(clean["commits"], 40);
    let tip = steps.iter().find(|s| s["kind"] == "tip").unwrap();
    assert_eq!(tip["commits"], 103);
    assert_eq!(tip["conflicts"], serde_json::json!(["b.txt"]));

    let ease = &report["data"]["ease"];
    assert_eq!(ease["clean"], 100);
    assert_eq!(ease["overlap"], 2);
    assert_eq!(ease["conflict"], 1);
    let commits = report["data"]["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 103);
    let ease_of = |sha: &str| {
        commits
            .iter()
            .position(|c| c["sha"] == sha)
            .map(|i| (i, commits[i]["ease"].clone()))
    };
    let (at, rating) = ease_of(&diverge).unwrap();
    assert!(at >= 100 && rating == "overlap", "{at} {rating}");
    assert_eq!(ease_of(&conflict).unwrap(), (102, "conflict".into()));
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
    let inbox: Value = serde_json::from_str(&tether(&fx.fork.path, &["inbox"]).stdout).unwrap();
    assert_eq!(inbox["data"]["items"][0]["folder"], "Docs");
    let list: Value = serde_json::from_str(&tether(&fx.fork.path, &["folder"]).stdout).unwrap();
    assert_eq!(list["data"]["folders"][0]["count"], 1);
    assert_eq!(tether(&fx.fork.path, &["folder", "--unfile", "#1"]).code, 0);
    let inbox: Value = serde_json::from_str(&tether(&fx.fork.path, &["inbox"]).stdout).unwrap();
    assert!(inbox["data"]["items"][0].get("folder").is_none());
}
