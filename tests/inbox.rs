//! init → status → inbox on a downloaded fork, end to end.

mod support;

use serde_json::Value;
use support::{Fixture, tether};

/// A fork whose sync point is upstream's first commit, with two upstream
/// commits after it.
fn synced_fork() -> (Fixture, String, String, String) {
    let fx = Fixture::downloaded_fork();
    let first = fx.upstream.git(&["rev-parse", "HEAD"]);
    fx.upstream
        .write("core/lib.rs", "pub fn answer() -> u32 { 43 }\n");
    let second = fx.upstream.commit("Fix the answer (#12)");
    fx.upstream.write("docs/guide.md", "hello\n");
    let third = fx.upstream.commit("Add a guide (#13)");
    fx.fork.git(&["fetch", "-q", "upstream"]);
    let init = tether(&fx.fork.path, &["init", "--sync-point", &first]);
    assert_eq!(init.code, 0, "{}", init.stdout);
    (fx, first, second, third)
}

fn shas(items: &Value) -> Vec<String> {
    items
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["sha"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn init_writes_config_and_records_the_sync_point() {
    let (fx, first, ..) = synced_fork();
    let config = std::fs::read_to_string(fx.fork.path.join(".tether/config.toml")).unwrap();
    assert!(config.contains("remote = \"upstream\""), "{config}");
    let message = fx.fork.git(&["log", "-1", "--format=%B"]);
    assert!(
        message.contains(&format!("Upstream-Synced: {first}")),
        "{message}"
    );
    // The marker commit is empty: it moves the sync point, not the tree.
    let changed = fx.fork.git(&["diff", "--name-only", "HEAD~1", "HEAD"]);
    assert!(changed.is_empty(), "{changed}");
}

#[test]
fn status_says_sync_when_upstream_moved() {
    let (fx, first, ..) = synced_fork();
    // init leaves its config uncommitted: status asks for that first.
    let run = tether(&fx.fork.path, &["status"]);
    assert_eq!(run.json["data"]["phase"], "record", "{}", run.stdout);
    fx.fork.git(&["add", ".tether"]);
    fx.fork.git(&["commit", "-q", "-m", "Add tether config"]);
    let run = tether(&fx.fork.path, &["status"]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    assert_eq!(run.json["data"]["phase"], "behind");
    assert_eq!(run.json["data"]["behind"], 2);
    assert_eq!(run.json["data"]["sync_point"]["upstream"], first.as_str());
    assert_eq!(run.json["next"]["command"], "tether sync");
}

#[test]
fn inbox_lists_pending_newest_first_with_pr_numbers() {
    let (fx, _, second, third) = synced_fork();
    let run = tether(&fx.fork.path, &["inbox"]);
    assert_eq!(run.code, 0, "{}", run.stdout);
    let items = &run.json["data"]["items"];
    assert_eq!(shas(items), vec![third, second]);
    assert_eq!(items[0]["pr"], "13");
    assert_eq!(items[0]["state"], "pending");
}

#[test]
fn decisions_and_hand_applied_commits_change_the_state() {
    let (fx, _, second, third) = synced_fork();
    std::fs::create_dir_all(fx.fork.path.join(".tether/decisions")).unwrap();
    std::fs::write(
        fx.fork.path.join(format!(".tether/decisions/{third}.toml")),
        "status = \"ignored\"\nnote = \"we keep our own docs\"\n",
    )
    .unwrap();
    // Apply the first change by hand, keeping upstream's subject.
    fx.fork
        .write("core/lib.rs", "pub fn answer() -> u32 { 43 }\n");
    fx.fork.commit("Fix the answer (#12)");

    let run = tether(&fx.fork.path, &["inbox", "--all"]);
    let items = run.json["data"]["items"].as_array().unwrap().clone();
    let by_sha = |sha: &str| items.iter().find(|i| i["sha"] == sha).unwrap().clone();
    assert_eq!(by_sha(&third)["state"], "ignored");
    assert_eq!(by_sha(&third)["note"], "we keep our own docs");
    assert_eq!(by_sha(&second)["state"], "pending");
    assert_eq!(by_sha(&second)["applied_here"], true);

    let status = tether(&fx.fork.path, &["status"]);
    assert_eq!(status.json["data"]["applied_here"][0], second.as_str());
}

#[test]
fn review_areas_and_relocations_tag_inbox_items() {
    let (fx, _, second, third) = synced_fork();
    let config = std::fs::read_to_string(fx.fork.path.join(".tether/config.toml")).unwrap();
    let extra = "\n[[review]]\nname = \"docs\"\npaths = [\"docs/*\"]\n\n\
                 [relocations.\"core/lib.rs\"]\nkind = \"inert\"\nfork = [\"core/lib.rs\"]\n";
    std::fs::write(fx.fork.path.join(".tether/config.toml"), config + extra).unwrap();
    let run = tether(&fx.fork.path, &["inbox"]);
    assert_eq!(run.code, 0, "{}", run.stdout);
    let items = run.json["data"]["items"].as_array().unwrap();
    let by_sha = |sha: &str| items.iter().find(|i| i["sha"] == sha).unwrap();
    assert_eq!(by_sha(&third)["reviews"][0], "docs");
    assert_eq!(by_sha(&second)["ports"][0], "core/lib.rs");
}

#[test]
fn diverging_sync_points_are_refused() {
    let (fx, first, ..) = synced_fork();
    // A side branch upstream that the main line does not contain.
    fx.upstream.git(&["checkout", "-q", "-b", "side", &first]);
    fx.upstream.write("side.txt", "x\n");
    let side = fx.upstream.commit("side work");
    fx.upstream.git(&["checkout", "-q", "main"]);
    fx.fork.git(&["fetch", "-q", "upstream", "side"]);
    let main_tip = fx.fork.git(&["rev-parse", "upstream/main"]);
    for sha in [&main_tip, &side] {
        fx.fork.git(&[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "sync",
            "-m",
            &format!("Upstream-Synced: {sha}"),
        ]);
    }
    let run = tether(&fx.fork.path, &["status"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    assert_eq!(run.json["error"]["code"], "sync_points_diverge");
}

#[test]
fn a_bad_config_is_a_coded_error() {
    let (fx, ..) = synced_fork();
    std::fs::write(
        fx.fork.path.join(".tether/config.toml"),
        "[upstream]\nremote = 1\n",
    )
    .unwrap();
    let run = tether(&fx.fork.path, &["status"]);
    assert_eq!(run.code, 1);
    assert_eq!(run.json["error"]["code"], "bad_config");
}

#[test]
fn a_generic_merge_subject_is_not_taken_for_hand_applied_work() {
    let (fx, ..) = synced_fork();
    // Upstream lands a merge with git's default subject…
    fx.upstream
        .git(&["checkout", "-q", "-b", "topic", "HEAD~1"]);
    fx.upstream.write("topic.txt", "t\n");
    fx.upstream.commit("topic work");
    fx.upstream.git(&["checkout", "-q", "main"]);
    fx.upstream.git(&[
        "merge",
        "-q",
        "--no-ff",
        "-m",
        "Merge branch 'topic'",
        "topic",
    ]);
    fx.fork.git(&["fetch", "-q", "upstream"]);
    // …and the fork has an unrelated merge with the same subject.
    fx.fork.git(&["checkout", "-q", "-b", "topic"]);
    fx.fork.write("mine.txt", "m\n");
    fx.fork.commit("mine");
    fx.fork.git(&["checkout", "-q", "main"]);
    fx.fork.git(&[
        "merge",
        "-q",
        "--no-ff",
        "-m",
        "Merge branch 'topic'",
        "topic",
    ]);

    let run = tether(&fx.fork.path, &["inbox"]);
    let items = run.json["data"]["items"].as_array().unwrap();
    assert!(
        items.iter().all(|i| i.get("applied_here").is_none()),
        "{}",
        run.stdout
    );
}

#[test]
fn decide_records_and_clears_by_sha_prefix_or_pr() {
    let (fx, _, second, third) = synced_fork();
    let run = tether(
        &fx.fork.path,
        &["decide", "#13", "ignored", "--note", "our docs"],
    );
    assert_eq!(run.code, 0, "{}", run.stdout);
    let file = fx.fork.path.join(format!(".tether/decisions/{third}.toml"));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("status = \"ignored\"") && text.contains("our docs"),
        "{text}"
    );

    // "pending" is what the sync point already says: nothing extra is stored.
    let run = tether(&fx.fork.path, &["decide", &second[..8], "pending"]);
    assert_eq!(run.code, 0, "{}", run.stdout);
    assert!(
        !fx.fork
            .path
            .join(format!(".tether/decisions/{second}.toml"))
            .exists()
    );

    let run = tether(&fx.fork.path, &["decide", &third[..8], "--clear"]);
    assert_eq!(run.code, 0, "{}", run.stdout);
    assert!(!file.exists());
}

#[test]
fn decide_refuses_incomplete_evidence() {
    let (fx, _, second, third) = synced_fork();
    let config = std::fs::read_to_string(fx.fork.path.join(".tether/config.toml")).unwrap();
    let extra = "\n[[review]]\nname = \"docs\"\npaths = [\"docs/*\"]\n\n\
                 [relocations.\"core/lib.rs\"]\nkind = \"inert\"\nfork = [\"core/lib.rs\"]\n";
    std::fs::write(fx.fork.path.join(".tether/config.toml"), config + extra).unwrap();

    let no_evidence = tether(
        &fx.fork.path,
        &["decide", &second[..8], "ported", "--port", "core/lib.rs"],
    );
    assert_eq!(
        no_evidence.json["error"]["code"], "refused",
        "{}",
        no_evidence.stdout
    );
    let ok = tether(
        &fx.fork.path,
        &[
            "decide",
            &second[..8],
            "ported",
            "--port",
            "core/lib.rs",
            "--evidence",
            "tests pass",
        ],
    );
    assert_eq!(ok.code, 0, "{}", ok.stdout);

    let missing_dest = tether(
        &fx.fork.path,
        &[
            "decide",
            &third[..8],
            "adapted",
            "--review",
            "docs",
            "--problem",
            "p",
            "--evidence",
            "e",
        ],
    );
    assert_eq!(
        missing_dest.json["error"]["code"], "refused",
        "{}",
        missing_dest.stdout
    );
    let ok = tether(
        &fx.fork.path,
        &[
            "decide",
            &third[..8],
            "adapted",
            "--review",
            "docs",
            "--problem",
            "p",
            "--evidence",
            "e",
            "--dest",
            "core/lib.rs",
        ],
    );
    assert_eq!(ok.code, 0, "{}", ok.stdout);
}
