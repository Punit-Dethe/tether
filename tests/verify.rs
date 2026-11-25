//! M3's bar: a failure upstream shipped is told apart from one the merge
//! made, and only the second blocks the sync.
//!
//! The "test suite" is a shell one-liner that prints every `checks/*.txt`
//! file; each line starting `FAIL ` is one failing test. That keeps the
//! fixture free of any toolchain but the shell.

mod support;

use std::fs;

use support::{Fixture, Repo, tether, tether_with};

const AGENT: &[(&str, &str)] = &[("TETHER_ACTOR", "agent")];

fn suite() -> &'static str {
    if cfg!(windows) {
        r"type checks\*.txt"
    } else {
        "cat checks/*.txt"
    }
}

struct World {
    _fx: Fixture,
    fork: Repo,
}

/// Upstream's next commit adds `checks/up.txt` with `up_line`.
fn world(up_line: &str) -> World {
    let fx = Fixture::downloaded_fork();
    let root = fx.upstream.path.parent().unwrap().to_path_buf();
    let upstream = Repo::init(root.join("up-verify"));
    upstream.write("checks/base.txt", "ok base\n");
    let base = upstream.commit("upstream: initial");

    let fork = Repo::init(root.join("fork-verify"));
    fork.write("checks/base.txt", "ok base\n");
    fork.write("checks/fork.txt", "ok fork\n");
    fork.commit(&format!("Initial commit\n\nUpstream-Synced: {base}"));
    let url = upstream.path.to_string_lossy().replace('\\', "/");
    fork.git(&["remote", "add", "upstream", &url]);
    fs::create_dir_all(fork.path.join(".tether")).unwrap();
    fork.write(
        ".tether/config.toml",
        &format!(
            "[upstream]\nname = \"Up\"\n\n\
             [[verify.step]]\nname = \"test\"\nrun = '{}'\nprefix = \"FAIL \"\n",
            suite()
        ),
    );
    fork.commit("add tether config");

    upstream.write("checks/up.txt", up_line);
    upstream.commit("Add a check (#5)");
    World { _fx: fx, fork }
}

#[test]
fn a_failure_upstream_shipped_does_not_block_the_sync() {
    let w = world("FAIL upstream_bug\n");
    let start = tether_with(&w.fork.path, &["sync"], AGENT);
    assert_eq!(start.code, 2, "{}", start.stdout);
    assert_eq!(start.json["next"]["command"], "tether verify");

    // Committing before verifying is refused, and says why.
    let early = tether_with(&w.fork.path, &["sync", "--continue"], AGENT);
    assert_eq!(early.code, 2, "{}", early.stdout);
    assert_eq!(early.json["next"]["command"], "tether verify");

    let verify = tether_with(&w.fork.path, &["verify"], AGENT);
    assert_eq!(verify.code, 2, "{}", verify.stdout);
    let verdict = &verify.json["data"]["verdict"];
    assert_eq!(verdict["new"], serde_json::json!([]), "{}", verify.stdout);
    assert_eq!(
        verdict["inherited"],
        serde_json::json!(["test: upstream_bug"])
    );
    assert_eq!(verify.json["next"]["command"], "tether sync --continue");

    let done = tether_with(&w.fork.path, &["sync", "--continue"], AGENT);
    assert_eq!(done.code, 0, "{}", done.stdout);
    // The throwaway worktrees are gone again.
    let worktrees = w.fork.git(&["worktree", "list"]);
    assert_eq!(worktrees.lines().count(), 1, "{worktrees}");
}

#[test]
fn a_failure_the_merge_made_blocks_until_fixed() {
    let w = world("ok up\n");
    let start = tether_with(&w.fork.path, &["sync"], AGENT);
    assert_eq!(start.code, 2, "{}", start.stdout);
    // A bad resolution: something neither side had now fails.
    w.fork.write("checks/fork.txt", "FAIL merge_bug\n");
    w.fork.git(&["add", "checks/fork.txt"]);

    let verify = tether_with(&w.fork.path, &["verify"], AGENT);
    assert_eq!(verify.code, 2, "{}", verify.stdout);
    let verdict = &verify.json["data"]["verdict"];
    assert_eq!(verdict["new"], serde_json::json!(["test: merge_bug"]));
    assert_eq!(verify.json["next"]["kind"], "edit");

    let status = tether_with(&w.fork.path, &["status"], AGENT);
    assert_eq!(status.json["next"]["kind"], "edit", "{}", status.stdout);
    let blocked = tether_with(&w.fork.path, &["sync", "--continue"], AGENT);
    assert_eq!(blocked.code, 2, "{}", blocked.stdout);
    // An agent may not skip the gate; a person may.
    let skip = tether_with(
        &w.fork.path,
        &["sync", "--continue", "--skip-verify"],
        AGENT,
    );
    assert_eq!(skip.json["error"]["code"], "needs_human", "{}", skip.stdout);

    // Fix it: the next verify runs again (the tree changed) and passes.
    w.fork.write("checks/fork.txt", "ok fork\n");
    w.fork.git(&["add", "checks/fork.txt"]);
    let verify = tether_with(&w.fork.path, &["verify"], AGENT);
    assert_eq!(verify.json["data"]["verdict"]["new"], serde_json::json!([]));
    let done = tether_with(&w.fork.path, &["sync", "--continue"], AGENT);
    assert_eq!(done.code, 0, "{}", done.stdout);
}

#[test]
fn single_targets_run_alone_and_reuse_an_unchanged_tree() {
    let w = world("FAIL upstream_bug\n");
    let start = tether(&w.fork.path, &["sync"]);
    assert_eq!(start.code, 2, "{}", start.stdout);
    let up = tether(&w.fork.path, &["verify", "--upstream"]);
    assert_eq!(up.code, 0, "{}", up.stdout);
    assert_eq!(
        up.json["data"]["failures"],
        serde_json::json!(["test: upstream_bug"])
    );
    assert_eq!(up.json["data"]["cached"], false);
    let again = tether(&w.fork.path, &["verify", "--upstream"]);
    assert_eq!(again.json["data"]["cached"], true);
    let base = tether(&w.fork.path, &["verify", "--baseline"]);
    assert_eq!(base.json["data"]["failures"], serde_json::json!([]));
}

#[test]
fn verify_without_steps_says_how_to_configure_it() {
    let fx = Fixture::downloaded_fork();
    let run = tether(&fx.fork.path, &["verify"]);
    assert_eq!(run.code, 1);
    let code = run.json["error"]["code"].as_str().unwrap();
    assert!(
        code == "refused" || code == "not_configured",
        "{}",
        run.stdout
    );
}
