//! `[port_audit] deferred_queue`: a deferral counts only while it is queued.

mod support;

use std::fs;

use support::{Fixture, Repo, tether};

struct World {
    _fx: Fixture,
    fork: Repo,
    fix: String,
}

/// A synced upstream fix to a relocated file, with no port record yet.
fn world() -> World {
    let fx = Fixture::downloaded_fork();
    let root = fx.upstream.path.parent().unwrap().to_path_buf();
    let upstream = Repo::init(root.join("up-def"));
    upstream.write("engine.rs", "fn engine() {}\n");
    let base = upstream.commit("upstream: initial");
    upstream.write("engine.rs", "fn engine() { clamp(); }\n");
    let fix = upstream.commit("Clamp the engine (#5)");

    let fork = Repo::init(root.join("fork-def"));
    fork.write("engine.rs", "fn engine() {}\n");
    fork.write("engine2.rs", "fn engine2() {}\n");
    fork.write("DEFERRED.md", "| Commit | Why |\n| --- | --- |\n");
    fork.commit(&format!("Initial commit\n\nUpstream-Synced: {base}"));
    let url = upstream.path.to_string_lossy().replace('\\', "/");
    fork.git(&["remote", "add", "upstream", &url]);
    fork.git(&["fetch", "-q", "upstream"]);
    fs::create_dir_all(fork.path.join(".tether")).unwrap();
    fork.write(
        ".tether/config.toml",
        "[upstream]\nname = \"Up\"\n\n\
         [port_audit]\ndeferred_queue = \"DEFERRED.md\"\n\n\
         [relocations.\"engine.rs\"]\nkind = \"relocated\"\nfork = [\"engine2.rs\"]\n",
    );
    fork.commit("add tether config");
    // Sync the fix in, so the port audit requires a record for it.
    assert_eq!(tether(&fork.path, &["sync"]).code, 2);
    let done = tether(&fork.path, &["sync", "--continue"]);
    assert_eq!(done.code, 0, "{}", done.stdout);
    World { _fx: fx, fork, fix }
}

#[test]
fn a_deferral_needs_a_queue_row() {
    let w = world();
    let defer = [
        "decide",
        &w.fix[..12],
        "deferred",
        "--port",
        "engine.rs",
        "--evidence",
        "engine2 needs the clamp; scheduled",
    ];
    let refused = tether(&w.fork.path, &defer);
    assert_eq!(
        refused.json["error"]["code"], "refused",
        "{}",
        refused.stdout
    );

    // Queue it, and the same deferral counts.
    let queue = fs::read_to_string(w.fork.path.join("DEFERRED.md")).unwrap();
    w.fork.write(
        "DEFERRED.md",
        &format!("{queue}| `{}` — clamp | port into engine2 |\n", w.fix),
    );
    let recorded = tether(&w.fork.path, &defer);
    assert_eq!(recorded.code, 0, "{}", recorded.stdout);
    let check = tether(&w.fork.path, &["check"]);
    assert_eq!(
        check.json["data"]["ports"]["unverified"],
        serde_json::json!([]),
        "{}",
        check.stdout
    );

    // Dropping the row (without porting) makes the deferral stop counting.
    w.fork.write("DEFERRED.md", &queue);
    let check = tether(&w.fork.path, &["check"]);
    assert_eq!(
        check.json["data"]["ports"]["unverified"][0]["sha"],
        w.fix.as_str(),
        "{}",
        check.stdout
    );
}
