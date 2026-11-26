//! `tether ui`: the routes answer with the commands' own JSON envelopes and
//! refuse anything that is not a plain GET from this machine.

mod support;

use serde_json::Value;
use support::{Fixture, tether};
use tether::ui::handle;

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

fn json(dir: &std::path::Path, target: &str) -> Value {
    let response = handle(dir, "GET", target, HOST, PORT);
    assert_eq!(response.status, 200, "{target}");
    assert!(response.content_type.starts_with("application/json"));
    serde_json::from_slice(&response.body).unwrap()
}

#[test]
fn the_page_and_every_route_answer() {
    let fx = synced_fork();
    let dir = &fx.fork.path;
    let page = handle(dir, "GET", "/", HOST, PORT);
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
    let at = |method, target, host| handle(&dir, method, target, host, PORT).status;
    assert_eq!(at("GET", "/", Some("evil.example:7311")), 403);
    assert_eq!(at("GET", "/", Some("127.0.0.1:9999")), 403);
    assert_eq!(at("GET", "/", None), 403);
    assert_eq!(at("POST", "/api/status", HOST), 405);
    assert_eq!(at("GET", "/api/nope", HOST), 404);
    assert_eq!(at("GET", "/api/packet?item=HEAD~1", HOST), 400);
    assert_eq!(at("GET", "/", Some("localhost:7311")), 200);
}
