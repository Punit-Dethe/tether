//! Several upstreams in one repository: a library vendored into a subfolder
//! is tracked and synced as its own upstream, with its own sync point, next
//! to the main one, and neither disturbs the other.

mod support;

use std::fs;

use support::{Fixture, Repo, tether};
use tempfile::TempDir;

struct Two {
    fx: Fixture,
    _lib_dir: TempDir,
    lib: Repo,
    lib_first: String,
    main_point: String,
}

/// The main fork (synced), plus `lib` copied into `vendor/lib/`.
fn two_upstreams() -> Two {
    let fx = Fixture::downloaded_fork();
    let main_point = fx.upstream.git(&["rev-parse", "HEAD"]);
    let init = tether(&fx.fork.path, &["init", "--sync-point", &main_point]);
    assert_eq!(init.code, 0, "{}", init.stdout);
    fx.fork.commit("add tether");

    let lib_dir = TempDir::new().unwrap();
    let lib = Repo::init(lib_dir.path().join("lib"));
    lib.write("src/a.rs", "fn one() {}\n\nfn two() {}\n\nfn three() {}\n");
    let lib_first = lib.commit("lib: first");
    fx.fork.write(
        "vendor/lib/src/a.rs",
        "fn one() {}\n\nfn two() {}\n\nfn three() {}\n",
    );
    fx.fork.commit("vendor lib");
    let url = lib.path.to_string_lossy().replace('\\', "/");
    fx.fork.git(&["remote", "add", "lib", &url]);
    fx.fork.git(&["fetch", "-q", "lib"]);
    Two {
        fx,
        _lib_dir: lib_dir,
        lib,
        lib_first,
        main_point,
    }
}

#[test]
fn an_extra_upstream_needs_its_own_remote_and_the_main_one_first() {
    let two = two_upstreams();
    let same = tether(
        &two.fx.fork.path,
        &["-u", "lib", "init", "--prefix", "vendor/lib/"],
    );
    assert_eq!(same.code, 1, "{}", same.stdout);
    assert!(
        same.json["summary"].as_str().unwrap().contains("remote"),
        "{}",
        same.stdout
    );

    let bad = tether(&two.fx.fork.path, &["-u", "Lib X", "status"]);
    assert_eq!(bad.code, 1);
}

#[test]
fn a_vendored_upstream_syncs_into_its_folder_on_its_own_sync_point() {
    let two = two_upstreams();
    let fork = &two.fx.fork;
    let init = tether(
        &fork.path,
        &[
            "-u",
            "lib",
            "init",
            "--remote",
            "lib",
            "--prefix",
            "vendor/lib",
            "--sync-point",
            &two.lib_first,
        ],
    );
    assert_eq!(init.code, 0, "{}", init.stdout);
    assert_eq!(init.json["data"]["upstream"]["prefix"], "vendor/lib/");
    assert!(
        fork.path
            .join(".tether/upstreams/lib/config.toml")
            .is_file()
    );
    let marker = fork.git(&["log", "-1", "--format=%B"]);
    assert!(
        marker.contains(&format!("Upstream-Synced-lib: {}", two.lib_first)),
        "{marker}"
    );
    fork.commit("add lib upstream");

    // Upstream fixes one function and adds a file; the fork patched another.
    two.lib.write(
        "src/a.rs",
        "fn one() {}\n\nfn two() { fixed() }\n\nfn three() {}\n",
    );
    two.lib.write("src/b.rs", "fn new() {}\n");
    let lib_next = two.lib.commit("lib: fix two, add b");
    fork.git(&["fetch", "-q", "lib"]);
    fork.write(
        "vendor/lib/src/a.rs",
        "fn one() { ours() }\n\nfn two() {}\n\nfn three() {}\n",
    );
    fork.commit("patch vendored lib");

    let theirs = tether(&fork.path, &["-u", "lib", "status"]);
    assert_eq!(theirs.code, 2, "{}", theirs.stdout);
    assert_eq!(theirs.json["data"]["behind"], 1);
    assert_eq!(
        theirs.json["next"]["command"], "tether -u lib sync",
        "{}",
        theirs.stdout
    );

    // The main upstream is current; its status reports lib's work.
    let all = tether(&fork.path, &["status"]);
    assert_eq!(all.code, 2, "{}", all.stdout);
    assert_eq!(all.json["data"]["behind"], 0);
    assert_eq!(all.json["data"]["upstreams"][0]["name"], "lib");
    assert_eq!(all.json["next"]["command"], "tether -u lib sync");

    let preview = tether(&fork.path, &["-u", "lib", "sync", "--dry-run"]);
    assert_eq!(preview.code, 0, "{}", preview.stdout);
    assert_eq!(preview.json["data"]["conflicts"], serde_json::json!([]));

    let sync = tether(&fork.path, &["-u", "lib", "sync"]);
    assert_eq!(sync.code, 2, "{}", sync.stdout);
    assert_eq!(sync.json["data"]["conflicts"], serde_json::json!([]));
    let done = tether(&fork.path, &["-u", "lib", "sync", "--continue"]);
    assert_eq!(done.code, 0, "{}", done.stdout);

    // Both changes landed inside the folder; nothing at the top level.
    let a = fs::read_to_string(fork.path.join("vendor/lib/src/a.rs")).unwrap();
    assert!(a.contains("ours()") && a.contains("fixed()"), "{a}");
    assert!(fork.path.join("vendor/lib/src/b.rs").is_file());
    assert!(!fork.path.join("src/b.rs").exists());

    // Each upstream kept its own sync point.
    let lib_status = tether(&fork.path, &["-u", "lib", "status"]);
    assert_eq!(
        lib_status.json["data"]["sync_point"]["upstream"],
        lib_next.as_str()
    );
    assert_eq!(lib_status.code, 0, "{}", lib_status.stdout);
    let main_status = tether(&fork.path, &["status"]);
    assert_eq!(
        main_status.json["data"]["sync_point"]["upstream"],
        two.main_point.as_str()
    );
    assert_eq!(main_status.code, 0, "{}", main_status.stdout);
}
