//! M2's bar: an agent finishes a sync by following `next` and nothing else.
//!
//! The driver below knows nothing about tether's commands. It runs
//! `tether status`, does what `next.kind` says, and repeats. It fills
//! placeholders the way AGENT.md tells an agent to, and it fails the test if
//! tether ever asks for a person or stops making progress.
//!
//! The fixture raises every M2 situation at once: a real conflict in a moved
//! file, an owned area that resolves itself but needs a review record, and
//! a relocated file whose fix needs a port record.

mod support;

use std::fs;

use serde_json::Value;
use support::{Fixture, Repo, Run, tether_with};

const LIB: &str = "fn one() {}\n\nfn two() {}\n\nfn three() {}\n";
const AGENT: &[(&str, &str)] = &[("TETHER_ACTOR", "agent")];

struct World {
    _fx: Fixture,
    fork: Repo,
}

fn world() -> World {
    let fx = Fixture::downloaded_fork();
    let root = fx.upstream.path.parent().unwrap().to_path_buf();
    let upstream = Repo::init(root.join("up-agent"));
    upstream.write("core/lib.rs", LIB);
    upstream.write("core/engine.rs", "fn engine() {}\n");
    upstream.write("ui/app.txt", "upstream ui\n");
    let base = upstream.commit("upstream: initial");

    let fork = Repo::init(root.join("fork-agent"));
    fork.git(&["config", "merge.directoryRenames", "true"]);
    fork.write(
        "handy/lib.rs",
        &LIB.replace("fn one() {}", "fn one() { fork(); }"),
    );
    // The fork rewrote the engine in its own file; upstream's copy is inert.
    fork.write("engine2.rs", "fn engine2() {}\n");
    fork.write("ui/app.txt", "our own ui\n");
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
         [[review]]\nname = \"ui\"\npaths = [\"ui/*\"]\n\n\
         [relocations.\"core/engine.rs\"]\nkind = \"relocated\"\nfork = [\"engine2.rs\"]\n\
         why = \"the fork rewrote the engine\"\n",
    );
    fork.commit("add tether config");

    // Upstream moves on: a fix that collides with the fork's hook, a UI
    // change in the owned area, and a fix to the relocated engine.
    upstream.write(
        "core/lib.rs",
        &LIB.replace("fn one() {}", "fn one() { guard(); }"),
    );
    upstream.commit("Guard one (#21)");
    upstream.write("ui/app.txt", "upstream ui v2\n");
    upstream.commit("Refresh the UI (#22)");
    upstream.write("core/engine.rs", "fn engine() { clamp(); }\n");
    upstream.commit("Clamp the engine (#23)");
    // AGENT.md's first step: fetch, so status sees what upstream has now.
    fork.git(&["fetch", "-q", "upstream"]);
    World { _fx: fx, fork }
}

/// Split a command line into words, honouring double quotes.
fn words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let (mut quoted, mut started) = (false, false);
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            ' ' if !quoted => {
                if started {
                    out.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            _ => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(word);
    }
    out
}

/// What an agent writes in place of each `<…>` placeholder, after reading
/// the packet. Choices take the first option that needs no destination
/// beyond the ones the packet names.
fn fill(command: &str, packet: &Value) -> String {
    let dest = packet["data"]["files"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|f| f["in_fork"] == true)
        .and_then(|f| f["fork_path"].as_str())
        .unwrap_or_default()
        .to_owned();
    let mut text = command.replace("[--dest <fork path>]", &format!("--dest {dest}"));
    while let Some(start) = text.find('<') {
        let end = start + text[start..].find('>').expect("placeholder closes");
        let inner = &text[start + 1..end];
        let value = if inner.contains('|') {
            match inner.split('|').next().unwrap() {
                "adapted" => "already-covered".to_owned(),
                first => first.to_owned(),
            }
        } else {
            format!("checked by the test agent ({inner})")
        };
        text.replace_range(start..=end, &value);
    }
    text
}

/// Run one `next.command` the way an agent's shell would.
fn run_command(fork: &Repo, line: &str) {
    for part in line.split(" && ") {
        let argv = words(part);
        let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
        match argv[0].as_str() {
            "tether" => {
                let run = tether_with(&fork.path, &args, AGENT);
                assert!(run.code != 1, "`{part}` failed:\n{}", run.stdout);
            }
            "git" => {
                fork.git(&args);
            }
            other => panic!("tether suggested a command an agent cannot run: {other}"),
        }
    }
}

fn packet(fork: &Repo, next: &Value) -> Value {
    let line = next["packet"]
        .as_str()
        .expect("edit/decide steps carry a packet");
    let argv = words(line);
    let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    let Run { code, json, stdout } = tether_with(&fork.path, &args, AGENT);
    assert_eq!(code, 0, "packet failed:\n{stdout}");
    json
}

#[test]
fn an_agent_syncs_by_following_next_only() {
    let w = world();
    let fork = &w.fork;
    let mut seen = Vec::new();

    for _ in 0..30 {
        let status = tether_with(&fork.path, &["status"], AGENT);
        assert!(status.code != 1, "status failed:\n{}", status.stdout);
        seen.push(
            status.json["data"]["phase"]
                .as_str()
                .unwrap_or("?")
                .to_owned(),
        );
        let next = &status.json["next"];
        if next.is_null() {
            assert_eq!(status.code, 0);
            break;
        }
        let command = next["command"].as_str().unwrap();
        match next["kind"].as_str().unwrap() {
            "run" => run_command(fork, command),
            "edit" => {
                let packet = packet(fork, next);
                assert_eq!(packet["data"]["kind"], "conflict", "{packet}");
                let path = packet["data"]["path"].as_str().unwrap();
                assert_eq!(path, "handy/lib.rs");
                // The packet shows both sides; keep the hook and the fix.
                assert!(
                    packet["data"]["upstream_diff"]
                        .as_str()
                        .unwrap()
                        .contains("guard();")
                );
                assert!(
                    packet["data"]["fork_diff"]
                        .as_str()
                        .unwrap()
                        .contains("fork();")
                );
                fork.write(
                    path,
                    &LIB.replace("fn one() {}", "fn one() { guard(); fork(); }"),
                );
                fork.git(&["add", path]);
                run_command(fork, command);
            }
            "decide" => {
                let packet = packet(fork, next);
                assert_eq!(packet["data"]["kind"], "commit", "{packet}");
                run_command(fork, &fill(command, &packet));
            }
            kind => panic!("tether asked for a person ({kind}): {next}"),
        }
    }

    assert_eq!(seen.last().map(String::as_str), Some("clean"), "{seen:?}");
    for phase in ["behind", "syncing", "checks", "record"] {
        assert!(
            seen.iter().any(|p| p == phase),
            "never went through {phase}: {seen:?}"
        );
    }
    // The sync landed with the hook and the fix, and the records are committed.
    let lib = fs::read_to_string(fork.path.join("handy/lib.rs")).unwrap();
    assert!(lib.contains("guard(); fork();"), "{lib}");
    assert_eq!(
        fs::read_to_string(fork.path.join("ui/app.txt")).unwrap(),
        "our own ui\n"
    );
    assert!(fork.git(&["status", "--porcelain"]).is_empty());
    let check = tether_with(&fork.path, &["check"], AGENT);
    assert_eq!(check.code, 0, "{}", check.stdout);
}

#[test]
fn reserved_actions_refuse_an_agent_but_not_a_person() {
    let w = world();
    let fork = &w.fork;
    let start = tether_with(&fork.path, &["sync"], AGENT);
    assert_eq!(start.code, 2, "{}", start.stdout);
    // Policy reserves abort here for a person.
    let config = fs::read_to_string(fork.path.join(".tether/config.toml")).unwrap();
    fs::write(
        fork.path.join(".tether/config.toml"),
        format!("{config}\n[agent]\nneeds_human = [\"abort\"]\n"),
    )
    .unwrap();
    let agent = tether_with(&fork.path, &["sync", "--abort"], AGENT);
    assert_eq!(agent.code, 1);
    assert_eq!(agent.json["error"]["code"], "needs_human");
    let person = tether_with(&fork.path, &["sync", "--abort"], &[]);
    assert_eq!(person.code, 0, "{}", person.stdout);
}
