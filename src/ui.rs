//! `tether ui`: a local, read-only dashboard over tether's own reports.
//!
//! A tiny standard-library HTTP server bound to loopback. It serves one
//! embedded page and a handful of JSON routes, each the exact `--json`
//! envelope of a read-only command, so the page shows nothing an agent
//! could not already read. Nothing here writes to the repository: no fetch,
//! no decide, no sync.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cli::Command;
use crate::commands;

const PAGE: &str = include_str!("ui/index.html");
/// Longest request head we read; anything bigger is not from our page.
const MAX_HEAD: usize = 16 * 1024;

/// One HTTP response, before it is written.
#[derive(Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl Response {
    fn json(status: u16, body: String) -> Self {
        Response {
            status,
            content_type: "application/json; charset=utf-8",
            body: body.into_bytes(),
        }
    }

    fn error(status: u16, message: &str) -> Self {
        Response::json(status, serde_json::json!({ "error": message }).to_string())
    }
}

/// Answer one request. `host` is the request's Host header and `port` the
/// port we listen on; any other host is refused so a web page elsewhere
/// cannot reach this server through DNS rebinding.
pub fn handle(dir: &Path, method: &str, target: &str, host: Option<&str>, port: u16) -> Response {
    let allowed = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    if !host.is_some_and(|h| allowed.iter().any(|a| a.eq_ignore_ascii_case(h))) {
        return Response::error(403, "unexpected host");
    }
    if method != "GET" {
        return Response::error(405, "read-only: only GET is served");
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let command = match path {
        "/" | "/index.html" => {
            return Response {
                status: 200,
                content_type: "text/html; charset=utf-8",
                body: PAGE.as_bytes().to_vec(),
            };
        }
        "/api/info" => {
            let root = crate::git::Git::new(dir)
                .toplevel()
                .unwrap_or_else(|_| dir.to_path_buf());
            let name = root.file_name().map(|n| n.to_string_lossy().into_owned());
            return Response::json(200, serde_json::json!({ "name": name }).to_string());
        }
        "/api/status" => Command::Status,
        "/api/inbox" => Command::Inbox {
            all: true,
            security: false,
        },
        "/api/map" => Command::Map {
            write: false,
            reason: None,
            why: None,
        },
        "/api/preview" => Command::Sync {
            target: None,
            dry_run: true,
            continue_: false,
            abort: false,
            no_fetch: true,
            skip_verify: false,
        },
        "/api/packet" => match query_param(query, "item").filter(|i| is_item(i)) {
            Some(item) => Command::Packet { item },
            None => return Response::error(400, "item must be a sha prefix or #PR"),
        },
        _ => return Response::error(404, "not found"),
    };
    Response::json(200, commands::run(&command, dir).render(true))
}

/// A sha prefix (4–40 hex digits) or a PR number, optionally `#`-prefixed.
fn is_item(item: &str) -> bool {
    let pr = item.strip_prefix('#').unwrap_or(item);
    let hex = (4..=40).contains(&item.len()) && item.bytes().all(|b| b.is_ascii_hexdigit());
    hex || (!pr.is_empty() && pr.len() <= 9 && pr.bytes().all(|b| b.is_ascii_digit()))
}

fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| value.replace("%23", "#"))
    })
}

/// Bind loopback, print the address, optionally open a browser, and serve
/// until the process is stopped. Returns only when binding fails.
pub fn serve(dir: &Path, port: u16, open: bool) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let port = listener.local_addr()?.port();
    let url = format!("http://127.0.0.1:{port}/");
    let mut out = std::io::stdout().lock();
    writeln!(out, "tether ui: {url}  (read-only; Ctrl+C to stop)")?;
    out.flush()?;
    drop(out);
    if open {
        open_browser(&url);
    }
    let dir: PathBuf = dir.to_path_buf();
    for stream in listener.incoming().flatten() {
        let dir = dir.clone();
        std::thread::spawn(move || {
            let _ = connection(stream, &dir, port);
        });
    }
    Ok(())
}

fn connection(stream: TcpStream, dir: &Path, port: u16) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new((&stream).take(MAX_HEAD as u64));
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut host = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("host")
        {
            host = Some(value.trim().to_string());
        }
    }
    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let response = handle(dir, method, target, host.as_deref(), port);
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        405 => "Method Not Allowed",
        _ => "Not Found",
    };
    let mut stream = stream;
    write!(
        stream,
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n\
         Content-Security-Policy: default-src 'none'; connect-src 'self'; \
         script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:\r\n\
         Connection: close\r\n\r\n",
        response.status,
        response.content_type,
        response.body.len()
    )?;
    stream.write_all(&response.body)?;
    stream.flush()
}

fn open_browser(url: &str) {
    let mut command = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    // No browser is fine: the address is already printed.
    let _ = command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_are_shas_or_prs() {
        assert!(is_item("1f2e9ea9"));
        assert!(is_item("#2565"));
        assert!(is_item("2565"));
        assert!(!is_item("abc"));
        assert!(!is_item("HEAD~1"));
        assert!(!is_item("#"));
        assert!(!is_item("../x"));
    }

    #[test]
    fn query_values_decode_the_pr_hash() {
        assert_eq!(
            query_param("item=%232565", "item").as_deref(),
            Some("#2565")
        );
        assert_eq!(
            query_param("a=1&item=ab12", "item").as_deref(),
            Some("ab12")
        );
        assert_eq!(query_param("a=1", "item"), None);
    }
}
