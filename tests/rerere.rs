//! Shared rerere resolutions and per-sync merge settings ([sync]).

mod support;

use std::fs;

use support::{Fixture, Repo, tether};

struct World {
    _fx: Fixture,
    fork: Repo,
    base: String,
}

/// Upstream and fork both change `lib.txt` line 1: a real conflict.
fn world() -> World {
    let fx = Fixture::downloaded_fork();
    let root = fx.upstream.path.parent().unwrap().to_path_buf();
    let upstream = Repo::init(root.join("up-rr"));
    upstream.write("lib.txt", "one\ntwo\nthree\n");
    upstream.write("keep.txt", "upstream keep\n");
    let base = upstream.commit("upstream: initial");

    let fork = Repo::init(root.join("fork-rr"));
    fork.write("lib.txt", "one (fork)\ntwo\nthree\n");
    fork.write("keep.txt", "fork keep\n");
    fork.write(".gitattributes", "keep.txt merge=ours\n");
    fork.commit(&format!("Initial commit\n\nUpstream-Synced: {base}"));
    let url = upstream.path.to_string_lossy().replace('\\', "/");
    fork.git(&["remote", "add", "upstream", &url]);
    fs::create_dir_all(fork.path.join(".tether")).unwrap();
    fork.write(
        ".tether/config.toml",
        "[upstream]\nname = \"Up\"\n\n\
         [relocations.\"keep.txt\"]\nkind = \"inert\"\nfork = [\"keep.txt\"]\n\n\
         [sync]\nrerere_cache = \".tether/rr-cache\"\n\
         [sync.git_config]\n\"merge.ours.driver\" = \"true\"\n",
    );
    fork.commit("add tether config");
    let base = fork.git(&["rev-parse", "HEAD"]);

    upstream.write("lib.txt", "one (upstream)\ntwo\nthree\n");
    upstream.write("keep.txt", "upstream keep v2\n");
    upstream.commit("Change one (#2)");
    World {
        _fx: fx,
        fork,
        base,
    }
}

#[test]
fn a_resolution_is_shared_and_replayed_in_a_fresh_clone() {
    let w = world();
    let fork = &w.fork;
    let start = tether(&fork.path, &["sync"]);
    assert_eq!(
        start.json["data"]["conflicts"],
        serde_json::json!(["lib.txt"]),
        "{}",
        start.stdout
    );
    // `merge=ours` held because [sync] git_config defined the driver.
    assert_eq!(
        fs::read_to_string(fork.path.join("keep.txt")).unwrap(),
        "fork keep\n"
    );

    fork.write("lib.txt", "one (fork + upstream)\ntwo\nthree\n");
    fork.git(&["add", "lib.txt"]);
    let done = tether(&fork.path, &["sync", "--continue"]);
    assert_eq!(done.code, 0, "{}", done.stdout);
    assert_eq!(done.json["data"]["rerere_saved"], 1, "{}", done.stdout);
    assert!(
        done.json["next"]["command"]
            .as_str()
            .unwrap()
            .starts_with("git add .tether/rr-cache")
    );
    let shared: Vec<_> = fs::read_dir(fork.path.join(".tether/rr-cache"))
        .unwrap()
        .collect();
    assert_eq!(shared.len(), 1);

    // Commit the shared cache, then play a fresh clone: back at the old
    // sync point with only the committed cache, no local rr-cache.
    fork.git(&["add", ".tether/rr-cache"]);
    fork.git(&["commit", "-qm", "Share new conflict resolutions"]);
    let shared_at = fork.git(&["rev-parse", "HEAD"]);
    fork.git(&["reset", "-q", "--hard", &w.base]);
    fork.git(&["checkout", "-q", &shared_at, "--", ".tether/rr-cache"]);
    fork.git(&["commit", "-qm", "bring the shared cache"]);
    fs::remove_dir_all(fork.path.join(".git/rr-cache")).unwrap();

    let again = tether(&fork.path, &["sync"]);
    assert_eq!(again.json["data"]["rerere_restored"], 1, "{}", again.stdout);
    // rerere replayed the resolution; it stays unstaged for review.
    let text = fs::read_to_string(fork.path.join("lib.txt")).unwrap();
    assert_eq!(text, "one (fork + upstream)\ntwo\nthree\n");
    assert_eq!(
        again.json["data"]["conflicts"],
        serde_json::json!(["lib.txt"])
    );
}

#[test]
fn a_relocation_to_a_missing_fork_file_fails_check() {
    let w = world();
    w.fork.git(&["fetch", "-q", "upstream"]);
    fs::remove_file(w.fork.path.join("keep.txt")).unwrap();
    w.fork.git(&["commit", "-qam", "drop keep"]);
    let check = tether(&w.fork.path, &["check"]);
    assert_eq!(check.code, 2, "{}", check.stdout);
    assert_eq!(
        check.json["data"]["relocations"]["missing"],
        serde_json::json!(["keep.txt -> keep.txt"])
    );
    assert_eq!(check.json["next"]["kind"], "edit");
}
