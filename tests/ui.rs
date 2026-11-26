//! `tether ui`: the routes answer with the commands' own JSON envelopes and
//! refuse anything that is not a plain GET from this machine.

mod support;

use serde_json::Value;
use support::{Fixture, tether};
use tether::ui::{Request, Response, handle};

const PORT: u16 = 7311;
const HOST: Option<&str> = Some("127.0.0.1:7311");

fn synced_fork() -> Fixture {
    let fx = Fixture::downloaded_fork();
    let first = fx.upstream.git(&["rev-parse", "HEAD"]);
    fx.upstream
        .write("core/lib.rs", "pub fn answer() -> u32 { 43 }\n");
    fx.upstream.commit("Fix the answer (#12)");
    fx.fork.git(&["fetch", "-q", "upstream"]);
    let init = tether(&fx.fork.path, &["init", "--sync-point", &first]);
    assert_eq!(init.code, 0, "{}", init.stdout);
    fx
}

fn get(dir: &std::path::Path, target: &str, host: Option<&str>) -> Response {
    let request = Request {
        method: "GET",
        target,
        host,
        ..Request::default()
    };
    handle(dir, &request, PORT)
}

fn post(dir: &std::path::Path, body: &str, origin: Option<&str>, content_type: &str) -> Response {
    let request = Request {
        method: "POST",
        target: "/api/folder",
        host: HOST,
        origin,
        content_type: Some(content_type),
        body: body.as_bytes(),
    };
    handle(dir, &request, PORT)
}

fn json(dir: &std::path::Path, target: &str) -> Value {
    let response = get(dir, target, HOST);
    assert_eq!(response.status, 200, "{target}");
    assert!(response.content_type.starts_with("application/json"));
    serde_json::from_slice(&response.body).unwrap()
}

#[test]
fn the_page_and_every_route_answer() {
    let fx = synced_fork();
    let dir = &fx.fork.path;
    let page = get(dir, "/", HOST);
    assert_eq!(page.status, 200);
    assert!(page.content_type.starts_with("text/html"));

    let status = json(dir, "/api/status");
    assert_eq!(status["command"], "status");
    assert_eq!(status["data"]["behind"], 1);
    let inbox = json(dir, "/api/inbox");
    assert_eq!(inbox["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(json(dir, "/api/map")["command"], "map");
    let preview = json(dir, "/api/preview");
    assert_eq!(preview["command"], "sync");
    assert_eq!(preview["data"]["dry_run"], true);
    assert_eq!(json(dir, "/api/packet?item=%2312")["command"], "packet");
    assert!(json(dir, "/api/info")["name"].is_string());
    let plan = json(dir, "/api/plan");
    assert_eq!(plan["command"], "plan");
    assert_eq!(plan["data"]["behind"], 1);
}

#[test]
fn the_preview_changes_nothing() {
    let fx = synced_fork();
    let before = fx.fork.git(&["status", "--porcelain"]);
    let head = fx.fork.git(&["rev-parse", "HEAD"]);
    json(&fx.fork.path, "/api/preview");
    assert_eq!(fx.fork.git(&["status", "--porcelain"]), before);
    assert_eq!(fx.fork.git(&["rev-parse", "HEAD"]), head);
}

#[test]
fn only_local_gets_are_served() {
    let (_tmp, dir) = Fixture::empty_dir();
    let at = |method, target, host| {
        let request = Request {
            method,
            target,
            host,
            ..Request::default()
        };
        handle(&dir, &request, PORT).status
    };
    assert_eq!(at("GET", "/", Some("evil.example:7311")), 403);
    assert_eq!(at("GET", "/", Some("127.0.0.1:9999")), 403);
    assert_eq!(at("GET", "/", None), 403);
    assert_eq!(at("POST", "/api/status", HOST), 405);
    assert_eq!(at("GET", "/api/nope", HOST), 404);
    assert_eq!(at("GET", "/api/packet?item=HEAD~1", HOST), 400);
    assert_eq!(at("GET", "/", Some("localhost:7311")), 200);
}

/// One raw HTTP exchange with a running server.
fn exchange(port: u16, request: &str) -> String {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn the_server_starts_answers_and_stops() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let fx = synced_fork();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tether"))
        .args(["ui", "--no-open"])
        .current_dir(&fx.fork.path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port: u16 = line
        .split("http://127.0.0.1:")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("no address in {line:?}"));

    let ok = exchange(
        port,
        &format!("GET /api/status HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    );
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
    assert!(ok.contains("Content-Security-Policy"), "{ok}");
    let body = ok.split("\r\n\r\n").nth(1).unwrap();
    let status: Value = serde_json::from_str(body).unwrap();
    assert_eq!(status["command"], "status");

    let rebind = exchange(
        port,
        "GET /api/status HTTP/1.1\r\nHost: evil.example\r\n\r\n",
    );
    assert!(rebind.starts_with("HTTP/1.1 403"), "{rebind}");
    // An oversized body is dropped without an answer.
    let huge = exchange(
        port,
        &format!(
            "POST /api/folder HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 999999\r\n\r\n"
        ),
    );
    assert!(huge.is_empty(), "{huge}");

    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
        "the port is released once tether ui stops"
    );
}

#[test]
fn the_page_can_file_commits_into_folders() {
    let fx = synced_fork();
    let dir = &fx.fork.path;
    let origin = Some("http://127.0.0.1:7311");
    let filed = post(
        dir,
        r##"{"name":"Fixes","items":["#12"]}"##,
        origin,
        "application/json",
    );
    assert_eq!(filed.status, 200);
    let report: Value = serde_json::from_slice(&filed.body).unwrap();
    assert_eq!(report["ok"], true, "{report}");
    let inbox = json(dir, "/api/inbox");
    assert_eq!(inbox["data"]["items"][0]["folder"], "Fixes");
    assert_eq!(inbox["data"]["folders"][0]["count"], 1);

    // Another site, or a form post, cannot write.
    let evil = post(
        dir,
        r#"{"name":"X"}"#,
        Some("https://evil.example"),
        "application/json",
    );
    assert_eq!(evil.status, 403);
    let form = post(dir, r#"{"name":"X"}"#, origin, "text/plain");
    assert_eq!(form.status, 403);
    let bad = post(
        dir,
        r#"{"name":"X","items":["HEAD~1"]}"#,
        origin,
        "application/json",
    );
    assert_eq!(bad.status, 400);

    let gone = post(
        dir,
        r#"{"name":"fixes","delete":true}"#,
        origin,
        "application/json",
    );
    assert_eq!(gone.status, 200);
    let inbox = json(dir, "/api/inbox");
    assert!(inbox["data"]["items"][0].get("folder").is_none());
    assert!(!fx.fork.path.join(".tether/folders.toml").exists());
}

#[test]
fn each_request_can_name_an_extra_upstream() {
    let fx = synced_fork();
    fx.fork.commit("add tether");
    let lib = support::Repo::init(fx.fork.path.parent().unwrap().join("lib"));
    lib.write("a.rs", "a\n");
    let first = lib.commit("lib: first");
    lib.write("b.rs", "b\n");
    lib.commit("lib: second");
    let url = lib.path.to_string_lossy().replace('\\', "/");
    fx.fork.git(&["remote", "add", "lib", &url]);
    fx.fork.git(&["fetch", "-q", "lib"]);
    let init = tether(
        &fx.fork.path,
        &[
            "-u",
            "lib",
            "init",
            "--remote",
            "lib",
            "--prefix",
            "vendor/lib/",
            "--sync-point",
            &first,
        ],
    );
    assert_eq!(init.code, 0, "{}", init.stdout);
    fx.fork.commit("track lib");

    let info = json(&fx.fork.path, "/api/info");
    let ups = info["upstreams"].as_array().unwrap();
    assert_eq!(ups.len(), 2, "{info}");
    assert_eq!(ups[0]["id"], Value::Null);
    assert_eq!(ups[1]["id"], "lib");
    assert_eq!(ups[1]["name"], "lib");
    assert_eq!(ups[1]["prefix"], "vendor/lib/");

    // The main upstream is 1 behind; lib is 1 behind on its own sync point.
    let main = json(&fx.fork.path, "/api/status");
    assert_eq!(main["data"]["upstreams"][0]["behind"], 1, "{main}");
    let extra = json(&fx.fork.path, "/api/status?upstream=lib");
    assert_eq!(
        extra["data"]["sync_point"]["upstream"],
        first.as_str(),
        "{extra}"
    );
    assert_eq!(extra["next"]["command"], "tether -u lib sync");

    assert_eq!(
        get(&fx.fork.path, "/api/status?upstream=Not%20ok", HOST).status,
        400
    );
}

#[test]
fn libraries_are_served_and_names_are_checked() {
    let fx = synced_fork();
    let deps = json(&fx.fork.path, "/api/deps");
    assert_eq!(deps["command"], "deps", "{deps}");
    assert!(deps["ok"].as_bool().unwrap());
    assert_eq!(get(&fx.fork.path, "/api/deps?name=a%20b", HOST).status, 400);
}
