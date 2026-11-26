//! `tether mcp`: the same commands as tools, for agents that speak the Model
//! Context Protocol.
//!
//! The agent's app starts `tether mcp` as a child process and talks to it over
//! stdin/stdout, one JSON-RPC message per line; it ends when stdin closes.
//! Each tool runs one tether command and answers with that command's own JSON
//! envelope, so a tool says exactly what `tether <command> --json` says.
//!
//! Whoever calls the tools is an agent: `[agent] needs_human` applies as if
//! `TETHER_ACTOR=agent` were set. Reading tools are always on. Tools that
//! change the fork (decide, sync, verify, closeout) exist only when the
//! person started the server with `--allow-writes`.

use std::io::{BufRead, Write};
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::cli::Command;
use crate::commands;
use crate::ui::is_item;

/// Protocol versions we speak, newest first.
const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "tether keeps this fork in step with its upstream. \
Start with `status`: its `next.command` is the one thing to do next. A command \
`tether <name> …` maps to the tool <name> (`tether sync --dry-run` is \
`sync_preview`). When `next.kind` is `ask_human`, or a tool fails with \
`needs_human`, stop and ask the person. Every tool answers with tether's JSON \
report: `ok`, `summary`, `data`, `next`, `error`.";

/// Serve until stdin closes.
pub fn serve(dir: &Path, allow_writes: bool) -> std::io::Result<()> {
    crate::autonomy::act_as_agent();
    let stdin = std::io::stdin().lock();
    let mut out = std::io::stdout().lock();
    for line in stdin.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = answer(dir, allow_writes, &line) {
            writeln!(out, "{reply}")?;
            out.flush()?;
        }
    }
    Ok(())
}

/// Answer one message; notifications get no answer.
pub fn answer(dir: &Path, allow_writes: bool, line: &str) -> Option<String> {
    let message: Value = match serde_json::from_str(line) {
        Ok(message) => message,
        Err(_) => return Some(error(Value::Null, -32700, "parse error")),
    };
    let id = message.get("id").cloned();
    let method = message.get("method").and_then(Value::as_str);
    let (Some(id), Some(method)) = (id, method) else {
        // A notification, or a response to a request we never send.
        return None;
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => Ok(initialize(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools(allow_writes) })),
        "tools/call" => call(dir, allow_writes, &params),
        _ => Err((-32601, format!("unknown method `{method}`"))),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
        Err((code, message)) => error(id, code, &message),
    })
}

fn error(id: Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked
        .filter(|v| VERSIONS.contains(v))
        .unwrap_or(VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "tether", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

struct Tool {
    name: &'static str,
    description: &'static str,
    /// JSON Schema `properties`.
    properties: fn() -> Value,
    required: &'static [&'static str],
    /// Changes the repository (or runs its build): needs `--allow-writes`.
    writes: bool,
}

fn none() -> Value {
    json!({})
}

const TOOLS: &[Tool] = &[
    Tool {
        name: "status",
        description: "Where the fork stands against upstream, and the one thing to do next (`next`). Start here.",
        properties: none,
        required: &[],
        writes: false,
    },
    Tool {
        name: "inbox",
        description: "Upstream commits with their state (pending, merged, ignored, unreleased), release, folder and security references. Pending only unless `all`.",
        properties: || {
            json!({
                "all": { "type": "boolean", "description": "Include merged, ignored and unreleased commits too." },
                "security": { "type": "boolean", "description": "Only commits that look like security fixes." }
            })
        },
        required: &[],
        writes: false,
    },
    Tool {
        name: "plan",
        description: "What to sync: the furthest step with no conflict for a person, each release, the tip, and pending commits rated easiest first (clean, owned, overlap, conflict).",
        properties: none,
        required: &[],
        writes: false,
    },
    Tool {
        name: "packet",
        description: "Everything needed to act on one upstream commit: its diff, the fork files it lands on, and how to record a decision.",
        properties: || json!({ "item": { "type": "string", "description": "A commit sha prefix or a PR number like #123." } }),
        required: &["item"],
        writes: false,
    },
    Tool {
        name: "sync_preview",
        description: "Preview a sync with no changes: which files would conflict for a person and which resolve themselves. Uses already-fetched upstream.",
        properties: || json!({ "target": { "type": "string", "description": "Upstream commit to preview up to (default: the sync target)." } }),
        required: &[],
        writes: false,
    },
    Tool {
        name: "map",
        description: "The divergence map: every fork change against upstream and why it exists, from intent markers in the code.",
        properties: none,
        required: &[],
        writes: false,
    },
    Tool {
        name: "check",
        description: "The CI gate: budgets, strays, owned areas, drift, port audit and reviews. Read-only.",
        properties: || json!({ "since": { "type": "string", "description": "Check reviews for everything synced since this fork revision." } }),
        required: &[],
        writes: false,
    },
    Tool {
        name: "doctor",
        description: "Check that git and this repository are usable, in plain words.",
        properties: none,
        required: &[],
        writes: false,
    },
    Tool {
        name: "deps",
        description: "Libraries the project takes from git instead of a registry, grouped as yours, maintained by your upstream, or someone else's. For your own forks: how far behind the project they came from, upstream security fixes, upstream changes to files your fork changed, and whether a merge would conflict. Read-only (fetches into a cache in .git).",
        properties: || {
            json!({
                "name": { "type": "string", "description": "One library (repository or package name): every upstream change, not just notable ones." },
                "no_fetch": { "type": "boolean", "description": "Use what was fetched last time." }
            })
        },
        required: &[],
        writes: false,
    },
    Tool {
        name: "prove",
        description: "Prove from the code that a sync kept upstream's changes: every hunk of every upstream commit since the previous sync point (or in the staged sync in progress) is checked against the fork with `git apply --check`. Each commit is proven (landed, superseded upstream, or covered by an owned area), explained by a recorded decision, dropped (missing, no decision), contradicted (recorded merged, missing) or unproven (the fork's code differs there). Read-only.",
        properties: || {
            json!({
                "from": { "type": "string", "description": "Upstream commit to prove from (default: the previous sync point)." }
            })
        },
        required: &[],
        writes: false,
    },
    Tool {
        name: "folder",
        description: "List folders, or group upstream commits into folders (a reading aid in .tether/folders.toml; it never changes a commit's state). With no arguments it lists them.",
        properties: || {
            json!({
                "name": { "type": "string", "description": "The folder to file into, rename or delete (created when filing)." },
                "items": { "type": "array", "items": { "type": "string" }, "description": "Commits to file into `name` (sha prefix or #PR)." },
                "unfile": { "type": "array", "items": { "type": "string" }, "description": "Commits to take out of their folder (without `name`)." },
                "rename": { "type": "string", "description": "New name for `name`." },
                "delete": { "type": "boolean", "description": "Delete `name`; its commits stay in the inbox." }
            })
        },
        required: &[],
        writes: false,
    },
    Tool {
        name: "decide",
        description: "Record a decision about one upstream commit. Read its `packet` first.",
        properties: || {
            json!({
                "item": { "type": "string", "description": "A commit sha prefix or #PR." },
                "outcome": { "type": "string", "enum": ["merged", "ignored", "pending", "ported", "not-applicable", "deferred", "adapted", "already-covered"] },
                "note": { "type": "string" },
                "port": { "type": "string", "description": "Relocated upstream file this port record is for." },
                "review": { "type": "string", "description": "Review area this review record is for." },
                "problem": { "type": "string", "description": "What upstream fixed (reviews)." },
                "evidence": { "type": "string", "description": "How the decision was checked." },
                "destinations": { "type": "array", "items": { "type": "string" }, "description": "Fork paths that carry the change (reviews)." },
                "clear": { "type": "boolean", "description": "Remove every recorded decision for this commit." }
            })
        },
        required: &["item"],
        writes: true,
    },
    Tool {
        name: "sync",
        description: "Bring upstream in as one squashed sync commit. `start` prepares it (owned areas resolve themselves; conflicts are left for you), `continue` commits once conflicts are resolved and verify passes, `abort` drops it.",
        properties: || {
            json!({
                "action": { "type": "string", "enum": ["start", "continue", "abort"] },
                "target": { "type": "string", "description": "With start: upstream commit to sync to (default: the sync target)." }
            })
        },
        required: &["action"],
        writes: true,
    },
    Tool {
        name: "verify",
        description: "Build and test the result with the fork's configured commands; on failure also the fork before the sync and upstream as shipped, so only new failures block. Can take minutes.",
        properties: || {
            json!({
                "only": { "type": "string", "enum": ["baseline", "upstream", "result"], "description": "Run just one side." },
                "force": { "type": "boolean", "description": "Rerun even when the tree has not changed." }
            })
        },
        required: &[],
        writes: true,
    },
    Tool {
        name: "closeout",
        description: "Move the sync point over fully decided upstream commits without changing a file. Checks only unless `execute`.",
        properties: || {
            json!({
                "rev": { "type": "string", "description": "Upstream commit to move the sync point to." },
                "execute": { "type": "boolean", "description": "Record it (default: only check)." }
            })
        },
        required: &["rev"],
        writes: true,
    },
];

fn tools(allow_writes: bool) -> Vec<Value> {
    TOOLS
        .iter()
        .filter(|tool| allow_writes || !tool.writes)
        .map(|tool| {
            let destructive = matches!(tool.name, "sync" | "closeout");
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": {
                    "type": "object",
                    "properties": (tool.properties)(),
                    "required": tool.required,
                    "additionalProperties": false,
                },
                "annotations": {
                    "readOnlyHint": !tool.writes && tool.name != "folder",
                    "destructiveHint": destructive,
                    "idempotentHint": !tool.writes,
                    "openWorldHint": false,
                },
            })
        })
        .collect()
}

type Fail = (i64, String);

fn invalid(message: impl Into<String>) -> Fail {
    (-32602, message.into())
}

fn call(dir: &Path, allow_writes: bool, params: &Value) -> Result<Value, Fail> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("tools/call needs a tool `name`"))?;
    let empty = Map::new();
    let args = match params.get("arguments") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(args)) => args,
        Some(_) => return Err(invalid("`arguments` must be an object")),
    };
    let tool = TOOLS
        .iter()
        .find(|tool| tool.name == name)
        .ok_or_else(|| invalid(format!("unknown tool `{name}`")))?;
    if tool.writes && !allow_writes {
        return Ok(tool_error(&format!(
            "`{name}` changes the fork, and this server is read-only; a person can start it with `tether mcp --allow-writes`"
        )));
    }
    let schema = (tool.properties)();
    if let Some(unknown) = args.keys().find(|key| schema.get(key.as_str()).is_none()) {
        return Err(invalid(format!("`{name}` has no argument `{unknown}`")));
    }
    let command = match command(name, &Args(args)) {
        Ok(command) => command,
        Err(message) => return Ok(tool_error(&message)),
    };
    let report = commands::run(&command, dir);
    let text = serde_json::to_string(&report).expect("a report is always serialisable");
    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "isError": !report.ok,
    }))
}

/// A tool that ran but could not do what was asked.
fn tool_error(message: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

struct Args<'a>(&'a Map<String, Value>);

impl Args<'_> {
    fn text(&self, key: &str) -> Result<Option<String>, String> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            Some(_) => Err(format!("`{key}` must be a string")),
        }
    }

    fn flag(&self, key: &str) -> Result<bool, String> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(false),
            Some(Value::Bool(flag)) => Ok(*flag),
            Some(_) => Err(format!("`{key}` must be true or false")),
        }
    }

    fn list(&self, key: &str) -> Result<Vec<String>, String> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::Array(values)) => values
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect::<Option<_>>()
                .ok_or_else(|| format!("`{key}` must be a list of strings")),
            Some(_) => Err(format!("`{key}` must be a list of strings")),
        }
    }

    fn item(&self, key: &str) -> Result<String, String> {
        let item = self
            .text(key)?
            .ok_or_else(|| format!("`{key}` is required"))?;
        is_item(&item)
            .then_some(item)
            .ok_or_else(|| format!("`{key}` must be a sha prefix or #PR"))
    }

    fn items(&self, key: &str) -> Result<Vec<String>, String> {
        let items = self.list(key)?;
        match items.iter().find(|i| !is_item(i)) {
            Some(bad) => Err(format!("`{bad}` is not a sha prefix or #PR")),
            None => Ok(items),
        }
    }

    /// A git revision; never one git could read as an option.
    fn rev(&self, key: &str) -> Result<Option<String>, String> {
        let rev = self.text(key)?;
        match &rev {
            Some(rev)
                if rev.is_empty() || rev.starts_with('-') || rev.contains(char::is_whitespace) =>
            {
                Err(format!("`{key}` must be a git revision"))
            }
            _ => Ok(rev),
        }
    }
}

fn command(name: &str, args: &Args) -> Result<Command, String> {
    Ok(match name {
        "status" => Command::Status,
        "inbox" => Command::Inbox {
            all: args.flag("all")?,
            security: args.flag("security")?,
        },
        "plan" => Command::Plan,
        "packet" => Command::Packet {
            item: args.item("item")?,
        },
        "sync_preview" => Command::Sync {
            target: args.rev("target")?,
            dry_run: true,
            continue_: false,
            abort: false,
            no_fetch: true,
            skip_verify: false,
        },
        "map" => Command::Map {
            write: false,
            reason: None,
            why: None,
        },
        "check" => Command::Check {
            update: false,
            accept_growth: false,
            since: args.rev("since")?,
        },
        "doctor" => Command::Doctor,
        "prove" => Command::Prove {
            from: args.rev("from")?,
        },
        "deps" => Command::Deps {
            name: args.text("name")?,
            no_fetch: args.flag("no_fetch")?,
        },
        "folder" => {
            let name = args.text("name")?;
            let (items, unfile) = (args.items("items")?, args.items("unfile")?);
            let (rename, delete) = (args.text("rename")?, args.flag("delete")?);
            if name.is_some() && !unfile.is_empty() {
                return Err("`unfile` takes no `name`".into());
            }
            if !items.is_empty() && (rename.is_some() || delete) {
                return Err("filing, renaming and deleting are separate calls".into());
            }
            Command::Folder {
                name,
                items,
                unfile,
                rename,
                delete,
            }
        }
        "decide" => Command::Decide {
            item: args.item("item")?,
            outcome: args.text("outcome")?,
            note: args.text("note")?,
            port: args.text("port")?,
            review: args.text("review")?,
            problem: args.text("problem")?,
            evidence: args.text("evidence")?,
            destinations: args.list("destinations")?,
            clear: args.flag("clear")?,
        },
        "sync" => {
            let target = args.rev("target")?;
            let action = args.text("action")?.unwrap_or_default();
            if !matches!(action.as_str(), "start" | "continue" | "abort") {
                return Err("`action` must be start, continue or abort".into());
            }
            if target.is_some() && action != "start" {
                return Err("`target` goes with `start` only".into());
            }
            Command::Sync {
                target,
                dry_run: false,
                continue_: action == "continue",
                abort: action == "abort",
                no_fetch: false,
                skip_verify: false,
            }
        }
        "verify" => {
            let only = args.text("only")?;
            if only
                .as_deref()
                .is_some_and(|o| !matches!(o, "baseline" | "upstream" | "result"))
            {
                return Err("`only` must be baseline, upstream or result".into());
            }
            Command::Verify {
                baseline: only.as_deref() == Some("baseline"),
                upstream: only.as_deref() == Some("upstream"),
                result: only.as_deref() == Some("result"),
                force: args.flag("force")?,
            }
        }
        "closeout" => Command::Closeout {
            rev: args.rev("rev")?.ok_or("`rev` is required")?,
            execute: args.flag("execute")?,
            no_fetch: false,
        },
        _ => unreachable!("every tool has a command"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_maps_to_a_command() {
        let empty = Map::new();
        for tool in TOOLS {
            let mut args = empty.clone();
            for key in tool.required {
                let value = match *key {
                    "action" => "start",
                    "item" => "#1",
                    _ => "abc1234",
                };
                args.insert((*key).to_owned(), json!(value));
            }
            assert!(command(tool.name, &Args(&args)).is_ok(), "{}", tool.name);
        }
    }

    #[test]
    fn revisions_cannot_be_options() {
        let args: Map<String, Value> =
            serde_json::from_value(json!({ "target": "--output=x" })).unwrap();
        assert!(command("sync_preview", &Args(&args)).is_err());
    }
}
