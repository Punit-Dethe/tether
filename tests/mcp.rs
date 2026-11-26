//! `tether mcp`: the agent's app starts it, talks JSON-RPC over stdio, and
//! every tool answers with the command's own report. Read-only unless the
//! person passed --allow-writes.

mod support;

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Value, json};
use support::{Fixture, tether};

fn synced_fork() -> Fixture {
    let fx = Fixture::downloaded_fork();
    let first = fx.upstream.git(&["rev-parse", "HEAD"]);
    fx.upstream.write("docs/guide.md", "hello\n");
    fx.upstream.commit("Add a guide (#7)");
    fx.fork.git(&["fetch", "-q", "upstream"]);
    let init = tether(&fx.fork.path, &["init", "--sync-point", &first]);
    assert_eq!(init.code, 0, "{}", init.stdout);
    fx.fork.commit("add tether");
    fx
}

/// Start `tether mcp`, send `messages` one per line, close stdin, and
/// return every reply by id.
fn session(dir: &std::path::Path, flags: &[&str], messages: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tether"))
        .arg("mcp")
        .args(flags)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("TETHER_ACTOR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for message in messages {
        writeln!(stdin, "{message}").unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect()
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments } })
}

fn report(reply: &Value) -> Value {
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned()))
}

fn names(reply: &Value) -> Vec<String> {
    reply["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn an_agent_reads_the_fork_through_tools() {
    let fx = synced_fork();
    let replies = session(
        &fx.fork.path,
        &[],
        &[
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": { "protocolVersion": "2025-06-18", "capabilities": {},
                                "clientInfo": { "name": "test", "version": "0" } } }),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            call(3, "status", json!({})),
            call(4, "plan", json!({})),
            call(5, "packet", json!({ "item": "#7" })),
            call(6, "folder", json!({ "name": "Docs", "items": ["#7"] })),
            call(7, "inbox", json!({ "all": true })),
            call(8, "decide", json!({ "item": "#7", "outcome": "ignored" })),
            call(9, "packet", json!({ "item": "HEAD~1" })),
            call(10, "nope", json!({})),
            call(11, "status", json!({ "surprise": 1 })),
            json!({ "jsonrpc": "2.0", "id": 12, "method": "ping" }),
            json!({ "jsonrpc": "2.0", "id": 13, "method": "resources/list" }),
        ],
    );
    // One reply per request; the notification got none.
    assert_eq!(replies.len(), 13, "{replies:#?}");
    let by_id = |id: u64| replies.iter().find(|r| r["id"] == id).unwrap();

    let init = &by_id(1)["result"];
    assert_eq!(init["protocolVersion"], "2025-06-18");
    assert_eq!(init["serverInfo"]["name"], "tether");
    assert!(init["instructions"].as_str().unwrap().contains("status"));

    let tools = names(by_id(2));
    for tool in [
        "status",
        "inbox",
        "plan",
        "packet",
        "sync_preview",
        "map",
        "check",
        "doctor",
        "prove",
        "folder",
    ] {
        assert!(tools.contains(&tool.to_owned()), "{tool} in {tools:?}");
    }
    assert!(!tools.contains(&"sync".to_owned()), "read-only by default");
    assert!(!tools.contains(&"decide".to_owned()));

    let status = report(by_id(3));
    assert_eq!(status["command"], "status");
    assert_eq!(status["data"]["behind"], 1);
    assert_eq!(report(by_id(4))["command"], "plan");
    assert_eq!(report(by_id(5))["command"], "packet");
    assert_eq!(by_id(5)["result"]["isError"], false);
    assert_eq!(report(by_id(7))["data"]["items"][0]["folder"], "Docs");

    // Writes are refused, and say how a person turns them on.
    assert_eq!(by_id(8)["result"]["isError"], true);
    assert!(
        report(by_id(8))
            .as_str()
            .unwrap()
            .contains("--allow-writes")
    );
    assert_eq!(by_id(9)["result"]["isError"], true);
    assert_eq!(by_id(10)["error"]["code"], -32602);
    assert_eq!(by_id(11)["error"]["code"], -32602);
    assert_eq!(by_id(12)["result"], json!({}));
    assert_eq!(by_id(13)["error"]["code"], -32601);
    // Nothing a read did touched the work tree beyond the folder file.
    assert_eq!(
        fx.fork
            .git(&["status", "--porcelain", "--untracked-files=all"]),
        "?? .tether/folders.toml"
    );
}

#[test]
fn writes_need_the_flag_and_still_obey_the_policy() {
    let fx = synced_fork();
    // Reserve ignoring a commit for a person.
    let config = fx.fork.path.join(".tether/config.toml");
    let text: String = std::fs::read_to_string(&config)
        .unwrap()
        .lines()
        .map(|line| match line.starts_with("needs_human") {
            true => "needs_human = [\"ignored\"]\n".to_owned(),
            false => format!("{line}\n"),
        })
        .collect();
    assert!(text.contains("needs_human = [\"ignored\"]"), "{text}");
    std::fs::write(&config, text).unwrap();
    fx.fork.commit("reserve ignore");

    let replies = session(
        &fx.fork.path,
        &["--allow-writes"],
        &[
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
            call(
                2,
                "sync",
                json!({ "action": "start", "target": "--upload-pack=x" }),
            ),
            call(3, "sync", json!({ "action": "start" })),
            call(4, "sync", json!({ "action": "abort" })),
            call(5, "decide", json!({ "item": "#7", "outcome": "ignored" })),
            call(6, "decide", json!({ "item": "#7", "note": "looked at it" })),
        ],
    );
    let by_id = |id: u64| replies.iter().find(|r| r["id"] == id).unwrap();
    let tools = names(by_id(1));
    assert!(tools.contains(&"sync".to_owned()) && tools.contains(&"decide".to_owned()));

    assert_eq!(by_id(2)["result"]["isError"], true);
    let started = report(by_id(3));
    assert_eq!(started["command"], "sync", "{started}");
    assert_ne!(started["ok"], false, "{started}");
    assert_eq!(report(by_id(4))["ok"], true, "{}", report(by_id(4)));
    // The caller is an agent, so `[agent] needs_human` applies.
    let refused = report(by_id(5));
    assert_eq!(refused["error"]["code"], "needs_human", "{refused}");
    assert_eq!(report(by_id(6))["ok"], true, "{}", report(by_id(6)));
}
