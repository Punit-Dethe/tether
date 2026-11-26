//! `tether ui`: a local, read-only dashboard over tether's own reports.
//!
//! A tiny standard-library HTTP server bound to loopback. It serves one
//! embedded page and a handful of JSON routes, each the exact `--json`
//! envelope of a command, so the page shows nothing an agent could not
//! already read. The only write is filing commits into folders (a reading
//! aid in `.tether/folders.toml`): no fetch, no decide, no sync.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cli::Command;
use crate::commands;

const PAGE: &str = include_str!("ui/index.html");
/// Longest request head we read; anything bigger is not from our page.
const MAX_HEAD: usize = 16 * 1024;
/// Longest request body we accept (a folder change).
const MAX_BODY: usize = 64 * 1024;

/// One parsed HTTP request.
#[derive(Debug, Default)]
pub struct Request<'a> {
    pub method: &'a str,
    pub target: &'a str,
    pub host: Option<&'a str>,
    pub origin: Option<&'a str>,
    pub content_type: Option<&'a str>,
    pub body: &'a [u8],
}

/// A folder change from the page: the `tether folder` arguments.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FolderChange {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    items: Vec<String>,
    #[serde(default)]
    unfile: Vec<String>,
    #[serde(default)]
    rename: Option<String>,
    #[serde(default)]
    delete: bool,
}

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

/// Answer one request. `port` is the port we listen on. Any Host but ours is
/// refused, so a web page elsewhere cannot reach this server through DNS
/// rebinding; the one write also needs a JSON body and, when the browser
/// sends an Origin, our own, so another site cannot post to it.
pub fn handle(dir: &Path, request: &Request, port: u16) -> Response {
    let allowed = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    if !request
        .host
        .is_some_and(|h| allowed.iter().any(|a| a.eq_ignore_ascii_case(h)))
    {
        return Response::error(403, "unexpected host");
    }
    let target = request.target;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if request.method == "POST" && path == "/api/folder" {
        let same_origin = request.origin.is_none_or(|origin| {
            allowed
                .iter()
                .any(|a| origin.eq_ignore_ascii_case(&format!("http://{a}")))
        });
        let json = request
            .content_type
            .is_some_and(|t| t.trim_start().starts_with("application/json"));
        if !same_origin || !json {
            return Response::error(403, "folder changes come only from this page");
        }
        let change: FolderChange = match serde_json::from_slice(request.body) {
            Ok(change) => change,
            Err(_) => return Response::error(400, "malformed folder change"),
        };
        if !change
            .items
            .iter()
            .chain(&change.unfile)
            .all(|i| is_item(i))
        {
            return Response::error(400, "items must be sha prefixes or #PRs");
        }
        let command = Command::Folder {
            name: change.name,
            items: change.items,
            unfile: change.unfile,
            rename: change.rename,
            delete: change.delete,
        };
        return Response::json(200, commands::run(&command, dir).render(true));
    }
    if request.method != "GET" {
        return Response::error(405, "only GET, and POST /api/folder, are served");
    }
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
        "/api/plan" => Command::Plan,
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
    writeln!(out, "tether ui: {url}  (Ctrl+C to stop)")?;
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
    let (mut host, mut origin, mut content_type, mut length) = (None, None, None, 0usize);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let value = Some(value.trim().to_string());
            match name.trim().to_ascii_lowercase().as_str() {
                "host" => host = value,
                "origin" => origin = value,
                "content-type" => content_type = value,
                "content-length" => {
                    length = value.and_then(|v| v.parse().ok()).unwrap_or(0);
                }
                _ => {}
            }
        }
    }
    if length > MAX_BODY {
        return Ok(());
    }
    // The head reader may have buffered the start of the body.
    let mut body = reader.buffer().to_vec();
    if body.len() < length {
        let mut rest = Vec::new();
        (&stream)
            .take((length - body.len()) as u64)
            .read_to_end(&mut rest)?;
        body.extend(rest);
    }
    body.truncate(length);
    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let request = Request {
        method,
        target,
        host: host.as_deref(),
        origin: origin.as_deref(),
        content_type: content_type.as_deref(),
        body: &body,
    };
    let response = handle(dir, &request, port);
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
