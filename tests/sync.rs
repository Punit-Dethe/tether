//! Squash sync and close-out, end to end, on repos with no shared history.
//!
//! The fork moved upstream's `core/` to `handy/` and owns `ui/` outright, so
//! rename detection, owned areas and conflicts are all exercised for real.

mod support;

use std::fs;

use support::{Fixture, Repo, tether};

const LIB: &str = "fn one() {}\n\nfn two() {}\n\nfn three() {}\n";

struct World {
    _fx: Fixture,
    upstream: Repo,
    fork: Repo,
}

fn world() -> World {
    let fx = Fixture::downloaded_fork();
    let upstream = Repo::init(fx.upstream.path.parent().unwrap().join("up2"));
    upstream.write("core/lib.rs", LIB);
    upstream.write("ui/app.txt", "upstream ui\n");
    let base = upstream.commit("upstream: initial");

    let fork = Repo::init(fx.upstream.path.parent().unwrap().join("fork2"));
    fork.git(&["config", "merge.directoryRenames", "true"]);
    fork.write(
        "handy/lib.rs",
        &LIB.replace("fn one() {}", "fn one() { fork(); }"),
    );
    fork.write("ui/app.txt", "our own ui\n");
    fork.write("fork.rs", "fn fork() {}\n");
    fork.commit(&format!("Initial commit\n\nUpstream-Synced: {base}"));
    let url = upstream.path.to_string_lossy().replace('\\', "/");
    fork.git(&["remote", "add", "upstream", &url]);
    fork.git(&["fetch", "-q", "upstream"]);
    fs::create_dir_all(fork.path.join(".tether")).unwrap();
    fork.write(
        ".tether/config.toml",
        "[upstream]\nname = \"Up\"\nurl = \"https://example.invalid/up\"\n\n\
         [[moved]]\nfork = \"handy/\"\nupstream = \"core/\"\n\n\
         [[owned]]\npath = \"ui/\"\n\n\
         [[review]]\nname = \"ui\"\npaths = [\"ui/*\"]\n",
    );
    fork.commit("add tether config");
    World {
        _fx: fx,
        upstream,
        fork,
    }
}

#[test]
fn a_clean_sync_is_one_single_parent_commit_with_the_marker() {
    let w = world();
    w.upstream.write(
        "core/lib.rs",
        &LIB.replace("fn three() {}", "fn three() { fixed(); }"),
    );
    let target = w.upstream.commit("Fix three (#7)");
    let head = w.fork.git(&["rev-parse", "HEAD"]);

    let start = tether(&w.fork.path, &["sync"]);
    assert_eq!(start.code, 2, "{}", start.stdout);
    assert_eq!(start.json["data"]["conflicts"].as_array().unwrap().len(), 0);
    assert_eq!(start.json["next"]["command"], "tether sync --continue");

    let done = tether(&w.fork.path, &["sync", "--continue"]);
    assert_eq!(done.code, 0, "{}", done.stdout);
    // Rename detection carried the fix into the moved file; our hook stayed.
    let lib = fs::read_to_string(w.fork.path.join("handy/lib.rs")).unwrap();
    assert!(
        lib.contains("fn one() { fork(); }") && lib.contains("fixed();"),
        "{lib}"
    );
    // One parent, the old HEAD; the marker names the target; no graft left.
    let parents = w.fork.git(&["rev-list", "--parents", "-n", "1", "HEAD"]);
    assert_eq!(parents.split_whitespace().nth(1), Some(head.as_str()));
    assert_eq!(parents.split_whitespace().count(), 2);
    let message = w.fork.git(&["log", "-1", "--format=%B"]);
    assert!(message.starts_with("sync(upstream): Up "), "{message}");
    assert!(
        message.contains("Fix three (#7)")
            && message.contains(&format!("Upstream-Synced: {target}"))
    );
    assert!(w.fork.git(&["replace", "-l"]).is_empty());
    // Upstream's commit is not in the fork's history.
    assert!(!w.fork.git(&["log", "--format=%H"]).contains(&target));

    let status = tether(&w.fork.path, &["status"]);
    assert_eq!(status.code, 0, "{}", status.stdout);
}

#[test]
fn owned_areas_never_take_upstream_changes() {
    let w = world();
    w.upstream.write("ui/app.txt", "upstream ui v2\n");
    w.upstream
        .write("ui/new.txt", "brand new upstream screen\n");
    w.upstream.commit("Redesign the ui");

    let start = tether(&w.fork.path, &["sync"]);
    assert_eq!(start.code, 2, "{}", start.stdout);
    assert_eq!(
        start.json["data"]["conflicts"].as_array().unwrap().len(),
        0,
        "{}",
        start.stdout
    );
    assert_eq!(
        fs::read_to_string(w.fork.path.join("ui/app.txt")).unwrap(),
        "our own ui\n"
    );
    assert!(!w.fork.path.join("ui/new.txt").exists());
    tether(&w.fork.path, &["sync", "--continue"]);
    assert!(w.fork.git(&["status", "--porcelain"]).is_empty());
}

#[test]
fn conflicts_stop_the_sync_until_resolved() {
    let w = world();
    w.upstream.write(
        "core/lib.rs",
        &LIB.replace("fn one() {}", "fn one() { upstream(); }"),
    );
    w.upstream.commit("Change one");

    let dry = tether(&w.fork.path, &["sync", "--dry-run"]);
    assert_eq!(
        dry.json["data"]["conflicts"][0], "handy/lib.rs",
        "{}",
        dry.stdout
    );
    assert!(
        w.fork.git(&["status", "--porcelain"]).is_empty(),
        "dry run changed nothing"
    );

    let start = tether(&w.fork.path, &["sync"]);
    assert_eq!(start.code, 2);
    assert_eq!(start.json["data"]["conflicts"][0], "handy/lib.rs");

    let early = tether(&w.fork.path, &["sync", "--continue"]);
    assert_eq!(early.code, 2, "{}", early.stdout);
    assert_eq!(early.json["data"]["conflicts"][0], "handy/lib.rs");

    w.fork.write(
        "handy/lib.rs",
        &LIB.replace("fn one() {}", "fn one() { fork(); upstream(); }"),
    );
    w.fork.git(&["add", "handy/lib.rs"]);
    let done = tether(&w.fork.path, &["sync", "--continue"]);
    assert_eq!(done.code, 0, "{}", done.stdout);
}

#[test]
fn abort_restores_head_and_refuses_when_nothing_is_running() {
    let w = world();
    w.upstream.write(
        "core/lib.rs",
        &LIB.replace("fn one() {}", "fn one() { upstream(); }"),
    );
    w.upstream.commit("Change one");
    let head = w.fork.git(&["rev-parse", "HEAD"]);
    tether(&w.fork.path, &["sync"]);
    let abort = tether(&w.fork.path, &["sync", "--abort"]);
    assert_eq!(abort.code, 0, "{}", abort.stdout);
    assert_eq!(w.fork.git(&["rev-parse", "HEAD"]), head);
    assert!(w.fork.git(&["status", "--porcelain"]).is_empty());
    let again = tether(&w.fork.path, &["sync", "--abort"]);
    assert_eq!(again.json["error"]["code"], "refused");
}

#[test]
fn a_second_sync_measures_from_the_new_sync_point() {
    let w = world();
    w.upstream.write(
        "core/lib.rs",
        &LIB.replace("fn two() {}", "fn two() { a(); }"),
    );
    w.upstream.commit("first");
    tether(&w.fork.path, &["sync"]);
    tether(&w.fork.path, &["sync", "--continue"]);
    w.upstream.write(
        "core/lib.rs",
        &LIB.replace("fn two() {}", "fn two() { a(); b(); }"),
    );
    let second = w.upstream.commit("second");
    let start = tether(&w.fork.path, &["sync"]);
    assert_eq!(start.json["data"]["behind"], 1, "{}", start.stdout);
    let done = tether(&w.fork.path, &["sync", "--continue"]);
    assert_eq!(done.json["data"]["target"], second.as_str());
    let up_to_date = tether(&w.fork.path, &["sync"]);
    assert_eq!(up_to_date.code, 0);
    assert_eq!(up_to_date.json["data"]["behind"], 0);
}

#[test]
fn a_dirty_tree_is_refused() {
    let w = world();
    w.upstream.write("core/extra.rs", "x\n");
    w.upstream.commit("extra");
    w.fork.write("fork.rs", "fn fork() { edited(); }\n");
    let run = tether(&w.fork.path, &["sync"]);
    assert_eq!(run.json["error"]["code"], "refused", "{}", run.stdout);
}

#[test]
fn closeout_needs_every_commit_decided_and_reviewed() {
    let w = world();
    w.upstream.write(
        "core/lib.rs",
        &LIB.replace("fn two() {}", "fn two() { a(); }"),
    );
    let fix = w.upstream.commit("Fix two");
    w.upstream.write("ui/app.txt", "upstream ui v2\n");
    let ui = w.upstream.commit("Tweak ui");
    w.fork.git(&["fetch", "-q", "upstream"]);

    let check = tether(&w.fork.path, &["closeout", &ui, "--no-fetch"]);
    assert_eq!(check.code, 2, "{}", check.stdout);
    assert_eq!(check.json["data"]["undecided"].as_array().unwrap().len(), 2);

    tether(
        &w.fork.path,
        &["decide", &fix[..10], "merged", "--note", "applied by hand"],
    );
    tether(
        &w.fork.path,
        &["decide", &ui[..10], "ignored", "--note", "our ui"],
    );
    let check = tether(&w.fork.path, &["closeout", &ui, "--no-fetch"]);
    assert_eq!(
        check.json["data"]["missing_reviews"][0]["area"], "ui",
        "{}",
        check.stdout
    );

    let review = tether(
        &w.fork.path,
        &[
            "decide",
            &ui[..10],
            "not-applicable",
            "--review",
            "ui",
            "--problem",
            "spacing",
            "--evidence",
            "our ui has no such spacing",
        ],
    );
    assert_eq!(review.code, 0, "{}", review.stdout);
    w.fork.commit("record decisions");

    let ready = tether(&w.fork.path, &["closeout", &ui, "--no-fetch"]);
    assert_eq!(ready.code, 0, "{}", ready.stdout);
    let tree = w.fork.git(&["rev-parse", "HEAD^{tree}"]);
    let done = tether(&w.fork.path, &["closeout", &ui, "--no-fetch", "--execute"]);
    assert_eq!(done.code, 0, "{}", done.stdout);
    assert_eq!(w.fork.git(&["rev-parse", "HEAD^{tree}"]), tree);
    let status = tether(&w.fork.path, &["status"]);
    assert_eq!(status.json["data"]["behind"], 0, "{}", status.stdout);
}
