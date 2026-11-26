//! `tether prove`, end to end: each upstream commit since the last sync is
//! checked against the fork's code, and the verdict weighed against what
//! was recorded about it.
//!
//! The fork moved upstream's `core/` to `handy/` and owns `ui/`.

mod support;

use std::fs;

use serde_json::Value;
use support::{Fixture, Repo, tether};

const FIX_A: &str = "fn a() { checked(); }\n";
const FIX_B: &str = "fn b() { bounded(); }\n";

struct World {
    _fx: Fixture,
    upstream: Repo,
    fork: Repo,
    /// The upstream commits, by name.
    a: String,
    b: String,
    c: String,
    ui: String,
    new_v1: String,
    new_v2: String,
}

/// Upstream makes six changes after the sync point; the fork's next sync
/// keeps some, adapts one and silently loses one.
fn world() -> World {
    world_with("")
}

/// [`world`], with extra lines in the fork's tether config.
fn world_with(extra_config: &str) -> World {
    let fx = Fixture::downloaded_fork();
    let root = fx.upstream.path.parent().unwrap().to_owned();
    let upstream = Repo::init(root.join("up2"));
    upstream.write("core/a.rs", "fn a() {}\n");
    upstream.write("core/b.rs", "fn b() {}\n");
    upstream.write("core/lib.rs", "fn one() {}\n");
    upstream.write("ui/app.txt", "upstream ui\n");
    let base = upstream.commit("upstream: initial");

    let fork = Repo::init(root.join("fork2"));
    fork.write("handy/a.rs", "fn a() {}\n");
    fork.write("handy/b.rs", "fn b() {}\n");
    fork.write("handy/lib.rs", "fn one() { fork(); }\n");
    fork.write("ui/app.txt", "our own ui\n");
    fork.commit(&format!("Initial commit\n\nUpstream-Synced: {base}"));
    let url = upstream.path.to_string_lossy().replace('\\', "/");
    fork.git(&["remote", "add", "upstream", &url]);
    fs::create_dir_all(fork.path.join(".tether")).unwrap();
    fork.write(
        ".tether/config.toml",
        &format!(
            "[upstream]\nname = \"Up\"\nurl = \"https://example.invalid/up\"\n\n\
             [[moved]]\nfork = \"handy/\"\nupstream = \"core/\"\n\n\
             [[owned]]\npath = \"ui/\"\n{extra_config}"
        ),
    );
    fork.commit("add tether config");

    upstream.write("core/a.rs", FIX_A);
    let a = upstream.commit("Fix a (#1)");
    upstream.write("core/b.rs", FIX_B);
    let b = upstream.commit("Fix b (#2)");
    upstream.write("core/lib.rs", "fn one() { upstream(); }\n");
    let c = upstream.commit("Change one (#3)");
    upstream.write("ui/app.txt", "new upstream ui\n");
    let ui = upstream.commit("Restyle (#4)");
    upstream.write("core/new.rs", "v1\n");
    let new_v1 = upstream.commit("Add new (#5)");
    upstream.write("core/new.rs", "v2\n");
    let new_v2 = upstream.commit("Rewrite new (#6)");
    fork.git(&["fetch", "-q", "upstream"]);

    World {
        _fx: fx,
        upstream,
        fork,
        a,
        b,
        c,
        ui,
        new_v1,
        new_v2,
    }
}

/// The fork's sync, done by hand: keeps a and new.rs, adapts one, loses b.
fn sync_by_hand(w: &World) {
    w.fork.write("handy/a.rs", FIX_A);
    w.fork
        .write("handy/lib.rs", "fn one() { fork(); upstream_adapted(); }\n");
    w.fork.write("handy/new.rs", "v2\n");
    w.fork.commit(&format!(
        "sync(upstream): Up\n\nUpstream-Synced: {}",
        w.new_v2
    ));
}

fn commit<'a>(json: &'a Value, sha: &str) -> &'a Value {
    json["data"]["commits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["sha"] == sha)
        .unwrap_or_else(|| panic!("{sha} not proven: {json}"))
}

fn finding(json: &Value, sha: &str) -> String {
    commit(json, sha)["finding"].as_str().unwrap().to_owned()
}

#[test]
fn the_code_shows_what_a_sync_kept_lost_and_changed() {
    let w = world();
    sync_by_hand(&w);

    let run = tether(&w.fork.path, &["prove"]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    let json = &run.json;
    assert_eq!(json["data"]["counts"]["commits"], 6);

    // Landed through the moved folder.
    assert_eq!(finding(json, &w.a), "proven");
    assert_eq!(commit(json, &w.a)["verdict"], "landed");
    // The silently lost fix.
    assert_eq!(finding(json, &w.b), "dropped");
    assert_eq!(commit(json, &w.b)["verdict"], "missing");
    // Adapted: the fork has other code there, so it is not provable.
    assert_eq!(finding(json, &w.c), "unproven");
    assert_eq!(commit(json, &w.c)["verdict"], "changed");
    // The owned area is the fork's policy.
    assert_eq!(commit(json, &w.ui)["verdict"], "policy");
    assert_eq!(finding(json, &w.ui), "proven");
    // A later upstream commit replaced v1; v2 is in the fork.
    assert_eq!(commit(json, &w.new_v1)["verdict"], "superseded");
    assert_eq!(commit(json, &w.new_v2)["verdict"], "landed");

    // The lost fix comes first, with its packet.
    let short_b = &w.b[..12];
    assert_eq!(json["next"]["kind"], "edit");
    assert_eq!(json["next"]["packet"], format!("tether packet {short_b}"));
    assert!(
        json["summary"]
            .as_str()
            .unwrap()
            .starts_with("1 dropped, 1 unproven of 6")
    );
}

#[test]
fn a_decision_cannot_claim_what_the_code_does_not_show() {
    let w = world();
    let short_b = &w.b[..12];
    let short_c = &w.c[..12];

    // Recorded as merged ahead of the sync (a cherry-pick, say), then lost:
    // the code contradicts the record.
    let decide = tether(&w.fork.path, &["decide", short_b, "merged"]);
    assert_eq!(decide.code, 0, "{}", decide.stdout);
    sync_by_hand(&w);
    let run = tether(&w.fork.path, &["prove"]);
    assert_eq!(finding(&run.json, &w.b), "contradicted");

    // Explaining both gaps accounts for every commit.
    for (item, args) in [
        (short_b, vec!["ignored", "--note", "b is unused here"]),
        (short_c, vec!["--note", "one() calls upstream_adapted()"]),
    ] {
        let mut full = vec!["decide", item];
        full.extend(args);
        let decide = tether(&w.fork.path, &full);
        assert_eq!(decide.code, 0, "{}", decide.stdout);
    }
    let run = tether(&w.fork.path, &["prove"]);
    assert_eq!(run.code, 0, "{}", run.stdout);
    assert_eq!(finding(&run.json, &w.b), "explained");
    assert_eq!(finding(&run.json, &w.c), "explained");
    assert!(
        commit(&run.json, &w.c)["explained_by"]
            .as_str()
            .unwrap()
            .contains("upstream_adapted")
    );
    assert!(
        run.json["summary"]
            .as_str()
            .unwrap()
            .starts_with("All 6 upstream commits")
    );
    assert!(run.json["next"].is_null(), "{}", run.stdout);
}

#[test]
fn a_staged_sync_is_proven_before_it_is_committed() {
    let w = world();
    let sync = tether(&w.fork.path, &["sync", "--no-fetch"]);
    assert_eq!(sync.code, 2, "{}", sync.stdout);
    // Proving waits for every conflict to be resolved.
    let conflicts = w.fork.git(&["diff", "--name-only", "--diff-filter=U"]);
    assert!(!conflicts.is_empty(), "{}", sync.stdout);
    let refused = tether(&w.fork.path, &["prove"]);
    assert_ne!(refused.code, 0, "{}", refused.stdout);
    assert!(refused.stdout.contains("resolve"), "{}", refused.stdout);
    for file in conflicts.lines() {
        w.fork.git(&["add", file]);
    }

    let run = tether(&w.fork.path, &["prove"]);
    assert_eq!(run.json["data"]["staged"], true, "{}", run.stdout);
    // The merge carried b in this time; one() kept only the fork's code.
    assert_eq!(finding(&run.json, &w.b), "proven");
    assert_eq!(finding(&run.json, &w.c), "unproven");
    // Nothing was written: the staged files are as the merge left them.
    assert_eq!(
        fs::read_to_string(w.fork.path.join("handy/b.rs")).unwrap(),
        FIX_B
    );
    let _ = &w.upstream;
}

#[test]
fn prove_needs_a_previous_sync_or_a_start() {
    let w = world();
    let run = tether(&w.fork.path, &["prove"]);
    assert_ne!(run.code, 0);
    assert!(run.stdout.contains("--from"), "{}", run.stdout);

    // An explicit start is used as given.
    let base = w.upstream.git(&["rev-parse", &format!("{}^", w.a)]);
    sync_by_hand(&w);
    let run = tether(&w.fork.path, &["prove", "--from", &base]);
    assert_eq!(run.json["data"]["counts"]["commits"], 6, "{}", run.stdout);
}

#[test]
fn an_owned_area_on_a_review_list_waits_for_a_review() {
    let w = world_with("\n[[review]]\nname = \"ui\"\npaths = [\"ui/*\"]\n");
    sync_by_hand(&w);
    let short = &w.ui[..12];

    // The fork keeps its own ui, but upstream's change there needs a look.
    let run = tether(&w.fork.path, &["prove"]);
    assert_eq!(commit(&run.json, &w.ui)["verdict"], "review");
    assert_eq!(finding(&run.json, &w.ui), "unproven");
    assert_eq!(commit(&run.json, &w.ui)["review_areas"][0], "ui");

    // A note is not a review.
    let note = tether(&w.fork.path, &["decide", short, "--note", "looked"]);
    assert_eq!(note.code, 0, "{}", note.stdout);
    let run = tether(&w.fork.path, &["prove"]);
    assert_eq!(finding(&run.json, &w.ui), "unproven");

    let review = tether(
        &w.fork.path,
        &[
            "decide",
            short,
            "not-applicable",
            "--review",
            "ui",
            "--problem",
            "upstream restyled its ui",
            "--evidence",
            "our ui has no such style",
        ],
    );
    assert_eq!(review.code, 0, "{}", review.stdout);
    let run = tether(&w.fork.path, &["prove"]);
    assert_eq!(finding(&run.json, &w.ui), "explained", "{}", run.stdout);
}
