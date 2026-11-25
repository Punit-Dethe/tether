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
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(shared.len(), 1);
    // Stored compressed, so rename detection can never pair it with code.
    assert!(shared[0].join("preimage.z").is_file());
    assert!(!shared[0].join("preimage").exists());

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

/// A rerere preimage is a near copy of the file it came from, so when the
/// fork moved that file away, rename detection routes upstream's edits into
/// the cache. tether's state must keep the fork's bytes.
#[test]
fn upstream_edits_never_reach_the_shared_cache() {
    let fx = Fixture::downloaded_fork();
    let root = fx.upstream.path.parent().unwrap().to_path_buf();
    let upstream = Repo::init(root.join("up-cache"));
    let body: String = (0..40).map(|i| format!("line {i}\n")).collect();
    upstream.write("big.rs", &body);
    let base = upstream.commit("upstream: initial");

    let fork = Repo::init(root.join("fork-cache"));
    // The fork dropped big.rs; its cache holds a preimage of it.
    let preimage = body.replacen("line 3\n", "<<<<<<<\nline 3\n=======\nmine\n>>>>>>>\n", 1);
    fork.write(".tether/rr-cache/abc/preimage", &preimage);
    fork.write("other.rs", "fn other() {}\n");
    fork.commit(&format!("Initial commit\n\nUpstream-Synced: {base}"));
    let url = upstream.path.to_string_lossy().replace('\\', "/");
    fork.git(&["remote", "add", "upstream", &url]);
    fork.write(
        ".tether/config.toml",
        "[upstream]\nname = \"Up\"\n\n[sync]\nrerere_cache = \".tether/rr-cache\"\n",
    );
    fork.commit("add tether config");

    upstream.write("big.rs", &body.replace("line 30\n", "line 30 fixed\n"));
    upstream.commit("Fix line 30 (#3)");

    let sync = tether(&fork.path, &["sync"]);
    assert_eq!(sync.code, 2, "{}", sync.stdout);
    assert_eq!(
        fs::read_to_string(fork.path.join(".tether/rr-cache/abc/preimage")).unwrap(),
        preimage,
        "{}",
        sync.stdout
    );
    // The dropped upstream change is reported, never silently lost.
    assert_eq!(
        sync.json["data"]["state_reverted"],
        serde_json::json!([".tether/rr-cache/abc/preimage"]),
        "{}",
        sync.stdout
    );
    assert_eq!(sync.json["next"]["kind"], "edit");
}

/// Upstream changed a file the fork keeps at a moved home that rename
/// detection cannot pair: tether merges the change into the home.
fn moved_fixture(fork_home_body: &str, upstream_fix: &str) -> (Fixture, Repo) {
    const BASE: &str = "fn tray() {}\n// a\n// b\n// c\n// d\n// e\n";
    const CONFIG: &str = concat!(
        "[upstream]\nname = \"Up\"\n\n",
        "[[moved]]\nfork = \"src/handy/\"\nupstream = \"src/\"\n\n",
        "[relocations.\"src/tray.rs\"]\nkind = \"relocated\"\nfork = [\"src/grain_tray.rs\"]\n",
    );
    let fx = Fixture::downloaded_fork();
    let root = fx.upstream.path.parent().unwrap().to_path_buf();
    let upstream = Repo::init(root.join("up-moved"));
    upstream.write("src/tray.rs", BASE);
    let base = upstream.commit("upstream: initial");

    let fork = Repo::init(root.join("fork-moved"));
    fork.write("src/handy/tray.rs", fork_home_body);
    fork.commit(&format!("Initial commit\n\nUpstream-Synced: {base}"));
    let url = upstream.path.to_string_lossy().replace('\\', "/");
    fork.git(&["remote", "add", "upstream", &url]);
    fork.write(".tether/config.toml", CONFIG);
    fork.commit("add tether config");
    upstream.write("src/tray.rs", upstream_fix);
    upstream.commit("Fix the tray (#4)");
    (fx, fork)
}

/// Fork-only lines, enough to push the home past rename detection.
fn grain_lines() -> String {
    (0..12).map(|n| format!("fn grain_{n}() {{}}\n")).collect()
}

const UPSTREAM_FIX: &str = "fn tray() { fixed(); }\n// a\n// b\n// c\n// d\n// e\n";

fn read(fork: &Repo, path: &str) -> String {
    std::fs::read_to_string(fork.path.join(path)).unwrap()
}

/// Even when the upstream path is listed as relocated, a moved home that
/// exists takes the change: three-way merged, conflicts marked there.
#[test]
fn an_upstream_change_conflicting_at_the_moved_home_is_marked_there() {
    let home = format!(
        "fn tray() {{ grain(); }}\n// a\n// b\n// c\n// d\n// e\n{}",
        grain_lines()
    );
    let (_fx, fork) = moved_fixture(&home, UPSTREAM_FIX);
    let sync = tether(&fork.path, &["sync"]);
    assert_eq!(
        sync.json["data"]["conflicts"],
        serde_json::json!(["src/handy/tray.rs"]),
        "{}",
        sync.stdout
    );
    assert_eq!(
        sync.json["data"]["moved"],
        serde_json::json!(["src/tray.rs -> src/handy/tray.rs"])
    );
    let merged = read(&fork, "src/handy/tray.rs");
    assert!(
        merged.contains("<<<<<<< fork (src/handy/tray.rs)"),
        "{merged}"
    );
    assert!(
        merged.contains("fixed();") && merged.contains("grain();"),
        "{merged}"
    );
    assert!(!fork.path.join("src/tray.rs").exists());
    let packet = tether(&fork.path, &["packet", "src/handy/tray.rs"]);
    assert_eq!(
        packet.json["data"]["conflict"], "both-changed",
        "{}",
        packet.stdout
    );
}

#[test]
fn an_upstream_change_merges_cleanly_into_the_moved_home() {
    let home = format!(
        "fn tray() {{}}\n// a\n// b\n// c\n// d\n// e\n{}",
        grain_lines()
    );
    let (_fx, fork) = moved_fixture(&home, UPSTREAM_FIX);
    let sync = tether(&fork.path, &["sync"]);
    assert_eq!(
        sync.json["data"]["conflicts"],
        serde_json::json!([]),
        "{}",
        sync.stdout
    );
    assert_eq!(
        sync.json["data"]["moved"],
        serde_json::json!(["src/tray.rs -> src/handy/tray.rs"]),
        "{}",
        sync.stdout
    );
    assert_eq!(
        read(&fork, "src/handy/tray.rs"),
        format!("{UPSTREAM_FIX}{}", grain_lines())
    );
}
