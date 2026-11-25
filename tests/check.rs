//! `tether check`: budgets, strays, adoption, drift, port audit, reviews.

mod support;

use std::fs;

use support::{Fixture, Repo, tether};

const LIB: &str = "fn one() {}\n\nfn two() {}\n\nfn three() {}\n";

struct World {
    _fx: Fixture,
    upstream: Repo,
    fork: Repo,
}

/// Upstream has `core/` and `ui/`; the fork moved `core/` to `handy/` and its
/// ui to `app/ui/`, which it owns.
fn world() -> World {
    let fx = Fixture::downloaded_fork();
    let root = fx.upstream.path.parent().unwrap().to_owned();
    let upstream = Repo::init(root.join("up3"));
    upstream.write("core/lib.rs", LIB);
    upstream.write("core/llm.rs", "fn call() {}\n");
    upstream.write("ui/button.txt", "button\n");
    let base = upstream.commit("upstream: initial");

    let fork = Repo::init(root.join("fork3"));
    fork.git(&["config", "merge.directoryRenames", "true"]);
    fork.write(
        "handy/lib.rs",
        &LIB.replace("fn one() {}", "fn one() { hook(); }"),
    );
    fork.write("handy/llm.rs", "fn call() {}\n");
    fork.write("ours/llm_client.rs", "fn call() { better(); }\n");
    fork.write("app/ui/button.txt", "our button\n");
    fork.commit(&format!("Initial commit\n\nUpstream-Synced: {base}"));
    let url = upstream.path.to_string_lossy().replace('\\', "/");
    fork.git(&["remote", "add", "upstream", &url]);
    fork.git(&["fetch", "-q", "upstream"]);
    fs::create_dir_all(fork.path.join(".tether")).unwrap();
    fork.write(
        ".tether/config.toml",
        "[upstream]\nname = \"Up\"\n\n\
         [[moved]]\nfork = \"handy/\"\nupstream = \"core/\"\n\n\
         [[moved]]\nfork = \"app/ui/\"\nupstream = \"ui/\"\n\n\
         [[owned]]\npath = \"app/\"\n\n\
         [[review]]\nname = \"ui\"\npaths = [\"ui/*\"]\n\n\
         [budget]\nscope = [\"core/\", \"handy/\"]\n\n\
         [relocations.\"core/llm.rs\"]\nkind = \"inert\"\nfork = [\"ours/llm_client.rs\"]\n",
    );
    fork.commit("add tether config");
    World {
        _fx: fx,
        upstream,
        fork,
    }
}

fn check(w: &World, args: &[&str]) -> support::Run {
    let mut all = vec!["check"];
    all.extend_from_slice(args);
    tether(&w.fork.path, &all)
}

#[test]
fn budgets_start_empty_and_lock_in_with_update() {
    let w = world();
    let first = check(&w, &[]);
    assert_eq!(first.code, 2, "{}", first.stdout);
    let failures = first.json["data"]["budgets"]["failures"]
        .as_array()
        .unwrap();
    assert!(
        failures[0]
            .as_str()
            .unwrap()
            .starts_with("NEW divergence: handy/lib.rs"),
        "{failures:?}"
    );

    let refused = check(&w, &["--update"]);
    assert_eq!(
        refused.json["error"]["code"], "refused",
        "{}",
        refused.stdout
    );
    let accepted = check(&w, &["--update", "--accept-growth"]);
    assert_eq!(accepted.code, 0, "{}", accepted.stdout);
    let budgets = fs::read_to_string(w.fork.path.join(".tether/budgets.toml")).unwrap();
    assert!(budgets.contains("\"handy/lib.rs\" = 2"), "{budgets}");
    w.fork.commit("lock budgets");

    let clean = check(&w, &[]);
    assert_eq!(clean.code, 0, "{}", clean.stdout);

    // More fork code in an upstream-derived file is growth…
    w.fork.write(
        "handy/lib.rs",
        &LIB.replace("fn one() {}", "fn one() { hook(); more(); }\nfn extra() {}"),
    );
    w.fork.commit("feature creep");
    let grew = check(&w, &[]);
    assert_eq!(grew.code, 2, "{}", grew.stdout);
    assert!(
        grew.json["data"]["budgets"]["failures"][0]
            .as_str()
            .unwrap()
            .starts_with("GREW")
    );
    // …and moving it back out is an improvement that update locks in.
    w.fork.write("handy/lib.rs", LIB);
    w.fork.commit("move the feature out");
    let shrunk = check(&w, &[]);
    assert_eq!(shrunk.code, 0, "{}", shrunk.stdout);
    assert_eq!(shrunk.json["next"]["command"], "tether check --update");
}

#[test]
fn a_new_upstream_file_at_the_old_path_is_a_stray() {
    let w = world();
    check(&w, &["--update", "--accept-growth"]);
    w.fork.write("core/new.rs", "fn new() {}\n");
    w.fork
        .commit("lock budgets; a sync dropped a file at upstream's path");
    w.upstream.write("core/new.rs", "fn new() {}\n");
    w.upstream.commit("add new.rs");
    w.fork.git(&["fetch", "-q", "upstream"]);
    let run = check(&w, &[]);
    assert_eq!(
        run.json["data"]["strays"][0], "core/new.rs",
        "{}",
        run.stdout
    );
    assert!(
        run.json["next"]["command"]
            .as_str()
            .unwrap()
            .starts_with("git mv core/new.rs")
    );
}

#[test]
fn an_owned_file_turning_identical_to_upstream_is_adoption() {
    let w = world();
    check(&w, &["--update", "--accept-growth"]);
    w.fork.commit("lock budgets");
    w.fork.write("app/ui/button.txt", "button\r\n");
    w.fork.commit("oops, took upstream's button");
    let run = check(&w, &[]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    assert_eq!(run.json["data"]["adopted"]["new"][0], "app/ui/button.txt");
}

#[test]
fn synced_fixes_to_relocated_files_need_port_records() {
    let w = world();
    check(&w, &["--update", "--accept-growth"]);
    w.fork.commit("lock budgets");
    w.upstream.write("core/llm.rs", "fn call() { retry(); }\n");
    let fix = w.upstream.commit("Retry LLM calls");
    let sync = tether(&w.fork.path, &["sync"]);
    assert_eq!(
        sync.json["data"]["conflicts"].as_array().unwrap().len(),
        0,
        "{}",
        sync.stdout
    );
    tether(&w.fork.path, &["sync", "--continue"]);
    check(&w, &["--update", "--accept-growth"]);
    w.fork.commit("rebaseline");

    let run = check(&w, &[]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    assert_eq!(
        run.json["data"]["ports"]["unverified"][0]["sha"],
        fix.as_str()
    );
    let next = run.json["next"]["command"].as_str().unwrap();
    assert!(next.contains("--port core/llm.rs"), "{next}");

    let decided = tether(
        &w.fork.path,
        &[
            "decide",
            &fix[..10],
            "ported",
            "--port",
            "core/llm.rs",
            "--evidence",
            "retry added to ours/llm_client.rs",
        ],
    );
    assert_eq!(decided.code, 0, "{}", decided.stdout);
    let run = check(&w, &[]);
    assert_eq!(run.code, 0, "{}", run.stdout);
    assert_eq!(run.json["data"]["ports"]["acknowledged"], 1);
}

#[test]
fn reviews_are_required_for_what_the_last_sync_brought_in() {
    let w = world();
    check(&w, &["--update", "--accept-growth"]);
    w.fork.commit("lock budgets");
    w.upstream.write("ui/button.txt", "button v2\n");
    let ui = w.upstream.commit("Restyle the button");
    let start = tether(&w.fork.path, &["sync"]);
    assert_eq!(start.code, 2, "{}", start.stdout);
    let done = tether(&w.fork.path, &["sync", "--continue"]);
    assert_eq!(done.code, 0, "{}", done.stdout);

    let run = check(&w, &[]);
    assert_eq!(
        run.json["data"]["reviews"]["missing"][0]["sha"],
        ui.as_str(),
        "{}",
        run.stdout
    );
    tether(
        &w.fork.path,
        &[
            "decide",
            &ui[..10],
            "not-applicable",
            "--review",
            "ui",
            "--problem",
            "contrast",
            "--evidence",
            "our button is a different component",
        ],
    );
    let run = check(&w, &[]);
    assert_eq!(run.code, 0, "{}", run.stdout);
    assert_eq!(run.json["data"]["reviews"]["required"], 1);
}
