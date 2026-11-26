//! Forked libraries: dependencies a project takes from git, not a registry.
//!
//! Found in `Cargo.toml` (with `Cargo.lock` for what is actually built),
//! `package.json` and `.gitmodules`, grouped by repository. Each one is:
//!
//! - **yours**: a fork you maintain (its owner is yours). tether fetches it
//!   and the project it came from into a cache inside `.git/tether/deps/`
//!   and reports how far behind it is, what upstream fixed, which upstream
//!   changes touch the files your fork changed, and whether a merge would
//!   conflict;
//! - **maintained by your upstream**: the project your repository forked
//!   pins the same repository, so keeping it current is their job (tagged,
//!   never hidden);
//! - **someone else's**.
//!
//! Pins that float (a branch, or no pin at all) are flagged, and checked
//! against the branch tip.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::git::Git;

/// How a manifest pins a git dependency.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Pin {
    /// An exact commit.
    Rev(String),
    Tag(String),
    /// Follows a branch: moves whenever the lockfile is refreshed.
    Branch(String),
    /// npm's `#ref`: a branch, tag or commit.
    Ref(String),
    /// No pin: follows the default branch.
    Default,
}

impl Pin {
    pub fn floats(&self) -> bool {
        matches!(self, Pin::Branch(_) | Pin::Ref(_) | Pin::Default)
    }

    pub fn text(&self) -> String {
        match self {
            Pin::Rev(rev) => format!("rev {}", short(rev)),
            Pin::Tag(tag) => format!("tag {tag}"),
            Pin::Branch(branch) => format!("branch {branch}"),
            Pin::Ref(reference) => format!("ref {reference}"),
            Pin::Default => "no pin".into(),
        }
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

fn is_hex(text: &str, min: usize) -> bool {
    text.len() >= min && text.len() <= 40 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// One place a manifest uses the repository.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Use {
    pub manifest: String,
    /// `dependencies`, `patch.crates-io`, `submodule`, …
    pub section: String,
    /// The crate, npm package or submodule path.
    pub package: String,
    pub pin: Pin,
    /// The commit actually built: from `Cargo.lock`, or the submodule's
    /// recorded commit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked: Option<String>,
}

/// A repository the project depends on, and every place it is used.
#[derive(Debug, Clone)]
pub struct Found {
    /// The address as the first manifest wrote it.
    pub url: String,
    pub uses: Vec<Use>,
}

/// A repository address in one comparable form: `https://host/owner/repo`,
/// lower case, without `.git`.
pub fn normalize(url: &str) -> String {
    let mut text = url.trim().trim_start_matches("git+").to_owned();
    if let Some(rest) = text.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            text = format!("https://{host}/{path}");
        }
    }
    for scheme in ["ssh://git@", "ssh://", "git://", "http://", "https://"] {
        if let Some(rest) = text.strip_prefix(scheme) {
            text = format!("https://{rest}");
            break;
        }
    }
    let text = text.trim_end_matches('/');
    let text = text.strip_suffix(".git").unwrap_or(text);
    text.to_ascii_lowercase()
}

/// `(host, owner, repo)` of a normalized address.
pub fn parts(key: &str) -> Option<(&str, &str, &str)> {
    let mut parts = key.strip_prefix("https://")?.split('/');
    let (host, owner, repo) = (parts.next()?, parts.next()?, parts.next()?);
    Some((host, owner, repo))
}

/// Every git dependency in a tree. `read` returns a file's text by
/// repository path; `gitlink` the commit a submodule path records.
pub fn scan(
    files: &[String],
    read: &dyn Fn(&str) -> Option<String>,
    gitlink: &dyn Fn(&str) -> Option<String>,
) -> Vec<Found> {
    let mut found: BTreeMap<String, Found> = BTreeMap::new();
    let mut add = |url: String, entry: Use| {
        found
            .entry(normalize(&url))
            .or_insert_with(|| Found {
                url,
                uses: Vec::new(),
            })
            .uses
            .push(entry);
    };
    let mut locks: HashMap<String, HashMap<(String, String), String>> = HashMap::new();
    for file in files {
        let name = file.rsplit('/').next().unwrap_or(file);
        let Some(text) = (match name {
            "Cargo.toml" | "package.json" => read(file),
            ".gitmodules" if file == ".gitmodules" => read(file),
            _ => None,
        }) else {
            continue;
        };
        match name {
            "Cargo.toml" => {
                let lock = cargo_lock_for(file, read, &mut locks);
                for (url, entry) in cargo_uses(file, &text, &lock) {
                    add(url, entry);
                }
            }
            "package.json" => {
                for (url, entry) in npm_uses(file, &text) {
                    add(url, entry);
                }
            }
            _ => {
                for (url, entry) in submodule_uses(&text, gitlink) {
                    add(url, entry);
                }
            }
        }
    }
    found.into_values().collect()
}

/// The `Cargo.lock` that governs `manifest`: the nearest one at or above it.
fn cargo_lock_for(
    manifest: &str,
    read: &dyn Fn(&str) -> Option<String>,
    cache: &mut HashMap<String, HashMap<(String, String), String>>,
) -> HashMap<(String, String), String> {
    let mut dir = manifest
        .rsplit_once('/')
        .map_or("", |(dir, _)| dir)
        .to_owned();
    loop {
        let path = if dir.is_empty() {
            "Cargo.lock".to_owned()
        } else {
            format!("{dir}/Cargo.lock")
        };
        if let Some(lock) = cache.get(&path) {
            return lock.clone();
        }
        if let Some(text) = read(&path) {
            let lock = parse_cargo_lock(&text);
            cache.insert(path, lock.clone());
            return lock;
        }
        if dir.is_empty() {
            return HashMap::new();
        }
        dir = dir.rsplit_once('/').map_or("", |(up, _)| up).to_owned();
    }
}

/// `(normalized repository, crate) -> commit` for every git package.
fn parse_cargo_lock(text: &str) -> HashMap<(String, String), String> {
    let mut out = HashMap::new();
    let Ok(lock) = text.parse::<toml::Table>() else {
        return out;
    };
    for package in lock
        .get("package")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(name), Some(source)) = (
            package.get("name").and_then(toml::Value::as_str),
            package.get("source").and_then(toml::Value::as_str),
        ) else {
            continue;
        };
        let Some(rest) = source.strip_prefix("git+") else {
            continue;
        };
        let Some((address, sha)) = rest.rsplit_once('#') else {
            continue;
        };
        let address = address.split('?').next().unwrap_or(address);
        out.insert((normalize(address), name.to_owned()), sha.to_owned());
    }
    out
}

fn cargo_uses(
    manifest: &str,
    text: &str,
    lock: &HashMap<(String, String), String>,
) -> Vec<(String, Use)> {
    let Ok(table) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    const KINDS: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];
    let mut sections: Vec<(String, &toml::Table)> = Vec::new();
    for kind in KINDS {
        if let Some(deps) = table.get(kind).and_then(toml::Value::as_table) {
            sections.push((kind.to_owned(), deps));
        }
    }
    for (cfg, target) in table
        .get("target")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flatten()
    {
        for kind in KINDS {
            if let Some(deps) = target.get(kind).and_then(toml::Value::as_table) {
                sections.push((format!("target.{cfg}.{kind}"), deps));
            }
        }
    }
    if let Some(deps) = table
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(toml::Value::as_table)
    {
        sections.push(("workspace.dependencies".into(), deps));
    }
    for (registry, deps) in table
        .get("patch")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flatten()
    {
        if let Some(deps) = deps.as_table() {
            sections.push((format!("patch.{registry}"), deps));
        }
    }
    let mut out = Vec::new();
    for (section, deps) in sections {
        for (key, spec) in deps {
            let Some(spec) = spec.as_table() else {
                continue;
            };
            let Some(url) = spec.get("git").and_then(toml::Value::as_str) else {
                continue;
            };
            let field = |name: &str| spec.get(name).and_then(toml::Value::as_str);
            let pin = if let Some(rev) = field("rev") {
                Pin::Rev(rev.to_owned())
            } else if let Some(tag) = field("tag") {
                Pin::Tag(tag.to_owned())
            } else if let Some(branch) = field("branch") {
                Pin::Branch(branch.to_owned())
            } else {
                Pin::Default
            };
            let package = field("package").unwrap_or(key).to_owned();
            let locked = lock.get(&(normalize(url), package.clone())).cloned();
            out.push((
                url.to_owned(),
                Use {
                    manifest: manifest.to_owned(),
                    section: section.clone(),
                    package,
                    pin,
                    locked,
                },
            ));
        }
    }
    out
}

/// A `package.json` dependency that comes from git, as `(address, pin)`.
pub fn npm_git(spec: &str) -> Option<(String, Pin)> {
    let (body, fragment) = match spec.split_once('#') {
        Some((body, fragment)) => (body, Some(fragment)),
        None => (spec, None),
    };
    let hosted = [
        ("github:", "github.com"),
        ("gitlab:", "gitlab.com"),
        ("bitbucket:", "bitbucket.org"),
    ];
    let url = if let Some((rest, host)) = hosted
        .iter()
        .find_map(|(prefix, host)| body.strip_prefix(prefix).map(|rest| (rest, host)))
    {
        format!("https://{host}/{rest}")
    } else if body.starts_with("git+")
        || body.starts_with("git://")
        || body.starts_with("git@")
        || (body.starts_with("https://") && body.ends_with(".git"))
    {
        body.to_owned()
    } else if body.matches('/').count() == 1
        && body
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && body
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    {
        format!("https://github.com/{body}")
    } else {
        return None;
    };
    let pin = match fragment.map(|f| f.trim_start_matches("semver:")) {
        None | Some("") => Pin::Default,
        Some(reference) if is_hex(reference, 7) => Pin::Rev(reference.to_owned()),
        Some(reference) => Pin::Ref(reference.to_owned()),
    };
    Some((url, pin))
}

fn npm_uses(manifest: &str, text: &str) -> Vec<(String, Use)> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for section in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ] {
        for (package, spec) in json
            .get(section)
            .and_then(serde_json::Value::as_object)
            .into_iter()
            .flatten()
        {
            let Some((url, pin)) = spec.as_str().and_then(npm_git) else {
                continue;
            };
            out.push((
                url,
                Use {
                    manifest: manifest.to_owned(),
                    section: section.to_owned(),
                    package: package.clone(),
                    pin,
                    locked: None,
                },
            ));
        }
    }
    out
}

fn submodule_uses(text: &str, gitlink: &dyn Fn(&str) -> Option<String>) -> Vec<(String, Use)> {
    let mut out = Vec::new();
    let mut current: Option<(Option<String>, Option<String>)> = None;
    let mut flush = |entry: Option<(Option<String>, Option<String>)>| {
        if let Some((Some(path), Some(url))) = entry {
            let locked = gitlink(&path);
            out.push((
                url,
                Use {
                    manifest: ".gitmodules".into(),
                    section: "submodule".into(),
                    pin: locked.clone().map_or(Pin::Default, Pin::Rev),
                    package: path,
                    locked,
                },
            ));
        }
    };
    for line in text.lines().map(str::trim) {
        if line.starts_with("[submodule") {
            flush(current.take());
            current = Some((None, None));
        } else if let (Some((key, value)), Some(entry)) = (line.split_once('='), current.as_mut()) {
            match key.trim() {
                "path" => entry.0 = Some(value.trim().to_owned()),
                "url" => entry.1 = Some(value.trim().to_owned()),
                _ => {}
            }
        }
    }
    flush(current);
    out
}

/// Scan the working tree of `git`'s repository.
pub fn scan_worktree(git: &Git, root: &Path) -> Result<Vec<Found>> {
    let files: Vec<String> = git
        .raw(&["ls-files", "-z"])?
        .split('\0')
        .filter(|f| !f.is_empty())
        .map(str::to_owned)
        .collect();
    let read = |path: &str| std::fs::read_to_string(root.join(path)).ok();
    let gitlink = |path: &str| gitlink_at(git, "HEAD", path);
    Ok(scan(&files, &read, &gitlink))
}

/// Scan the tree of commit `rev` (upstream's, for "who maintains this").
pub fn scan_tree(git: &Git, rev: &str) -> Result<Vec<Found>> {
    let files: Vec<String> = git
        .raw(&["ls-tree", "-r", "-z", "--name-only", rev])?
        .split('\0')
        .filter(|f| !f.is_empty())
        .map(str::to_owned)
        .collect();
    let read = |path: &str| git.raw(&["show", &format!("{rev}:{path}")]).ok();
    let gitlink = |path: &str| gitlink_at(git, rev, path);
    Ok(scan(&files, &read, &gitlink))
}

fn gitlink_at(git: &Git, rev: &str, path: &str) -> Option<String> {
    let out = git.ok(&["ls-tree", rev, "--", path]).ok()?;
    let mut fields = out.split_whitespace();
    (fields.next()? == "160000" && fields.next()? == "commit")
        .then(|| fields.next())?
        .map(str::to_owned)
}

// ---------------------------------------------------------------------------
// Analysis

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Whose {
    /// A fork you maintain.
    Yours,
    /// The project your repository forked pins it: theirs to keep current.
    Upstream,
    /// Someone else's.
    Other,
}

/// An upstream commit of one of your forks.
#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub sha: String,
    pub subject: String,
    pub timestamp: i64,
    /// Files it touches that your fork also changed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub near_yours: Vec<String>,
    /// GHSA/CVE ids and keywords.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub security: Vec<String>,
}

/// Where one of your forks stands against the project it came from.
#[derive(Debug, Clone, Serialize)]
pub struct ForkState {
    /// The project it was forked from.
    pub upstream: String,
    pub upstream_branch: String,
    /// The commit your project builds.
    pub pinned: String,
    /// Your fork's commits on top of upstream.
    pub ahead: usize,
    /// Upstream commits your pinned fork does not have.
    pub behind: usize,
    /// Files your fork changed.
    pub your_files: usize,
    /// Upstream releases the pinned fork does not contain, newest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub releases: Vec<String>,
    /// Files a merge of upstream would conflict on (`None`: not previewed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflicts: Option<Vec<String>>,
    /// Your fork has commits newer than the pin on this branch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newer_on: Option<Newer>,
    /// Upstream commits: all of them for one library, otherwise only the
    /// ones that need a look (security fixes and changes near yours).
    pub changes: Vec<Change>,
    pub security: usize,
    pub near_yours: usize,
    /// Your fork's commits on top of upstream, newest first (one library only).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub patches: Vec<Patch>,
    /// Files your fork changed (one library only).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

/// One of your fork's own commits.
#[derive(Debug, Clone, Serialize)]
pub struct Patch {
    pub sha: String,
    pub subject: String,
    pub timestamp: i64,
    /// How many of the fork's changed files it touches.
    pub files: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Newer {
    pub branch: String,
    pub commits: usize,
}

/// A floating pin checked against its branch.
#[derive(Debug, Clone, Serialize)]
pub struct Tip {
    /// `refs/heads/main`, or `HEAD`.
    pub reference: String,
    pub tip: String,
    /// The branch has moved past what the lockfile builds.
    pub moved: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Library {
    /// The repository's name.
    pub name: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub whose: Whose,
    /// "maintained by <upstream>" for libraries your upstream pins.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maintained_by: Option<String>,
    pub uses: Vec<Use>,
    /// `unpinned`, `floating`, `pinned_differently`, `differs_from_upstream`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<String>,
    /// How your upstream pins it, when that differs from you.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_pin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tip: Option<Tip>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork: Option<ForkState>,
    /// Why something could not be checked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

pub struct Options<'a> {
    pub fetch: bool,
    /// Check only this library, and report every upstream change it has.
    pub only: Option<&'a str>,
}

/// Classify and check every git dependency of the repository at `root`.
pub fn libraries(
    git: &Git,
    root: &Path,
    config: Option<&Config>,
    options: &Options,
) -> Result<Vec<Library>> {
    let found = scan_worktree(git, root)?;
    let ours = our_owners(git, config);
    // What the upstream project pins, keyed by repository.
    let upstream_pins: Option<(String, HashMap<String, BTreeSet<String>>)> = match config {
        Some(config) => match git.commit_sha(&config.upstream_ref())? {
            Some(tip) => {
                let mut pins: HashMap<String, BTreeSet<String>> = HashMap::new();
                for dep in scan_tree(git, &tip)? {
                    pins.entry(normalize(&dep.url))
                        .or_default()
                        .extend(dep.uses.iter().map(|u| u.pin.text()));
                }
                Some((config.upstream_name().to_owned(), pins))
            }
            None => None,
        },
        None => None,
    };
    let upstream_owner: Option<String> = config.and_then(|config| {
        let url = config
            .upstream
            .url
            .clone()
            .or_else(|| git.ok(&["remote", "get-url", &config.upstream.remote]).ok())?;
        parts(&normalize(&url)).map(|(_, owner, _)| owner.to_owned())
    });
    let cache_root = cache_root(git)?;
    let mut heads = Heads::load(&cache_root);
    let mut out = Vec::new();
    for dep in found {
        let key = normalize(&dep.url);
        let (owner, name) = match parts(&key) {
            Some((_, owner, repo)) => (Some(owner.to_owned()), repo.to_owned()),
            None => (None, key.rsplit('/').next().unwrap_or(&key).to_owned()),
        };
        let pins: BTreeSet<String> = dep.uses.iter().map(|u| u.pin.text()).collect();
        let mut flags = Vec::new();
        if dep.uses.iter().any(|u| u.pin == Pin::Default) {
            flags.push("unpinned".to_owned());
        } else if dep.uses.iter().any(|u| u.pin.floats()) {
            flags.push("floating".to_owned());
        }
        if pins.len() > 1 {
            flags.push("pinned_differently".to_owned());
        }
        // Yours: owned by you, or declared with `[[deps.fork]]` (naming a
        // fork's upstream means you maintain it).
        let declared = config
            .into_iter()
            .flat_map(|c| &c.deps.forks)
            .any(|f| normalize(&f.url) == key);
        let yours = declared
            || owner
                .as_deref()
                .is_some_and(|o| ours.iter().any(|mine| mine.eq_ignore_ascii_case(o)));
        let theirs = upstream_pins
            .as_ref()
            .and_then(|(name, pins)| pins.get(&key).map(|p| (name, p)));
        // A fork owned by whoever owns your upstream is theirs to maintain,
        // even when the upstream no longer pins it.
        let upstream_owned = owner
            .as_deref()
            .zip(upstream_owner.as_deref())
            .is_some_and(|(owner, theirs)| owner.eq_ignore_ascii_case(theirs));
        let (whose, maintained_by, upstream_pin) = match (yours, theirs) {
            (true, _) => (Whose::Yours, None, None),
            (false, None) if upstream_owned => (
                Whose::Upstream,
                config.map(|c| c.upstream_name().to_owned()),
                None,
            ),
            (false, Some((upstream, their_pins))) => {
                let differs = *their_pins != pins;
                if differs {
                    flags.push("differs_from_upstream".to_owned());
                }
                (
                    Whose::Upstream,
                    Some(upstream.clone()),
                    differs.then(|| their_pins.iter().cloned().collect::<Vec<_>>().join(", ")),
                )
            }
            (false, None) => (Whose::Other, None, None),
        };
        let mut library = Library {
            name,
            url: dep.url.clone(),
            owner,
            whose,
            maintained_by,
            uses: dep.uses.clone(),
            flags,
            upstream_pin,
            tip: None,
            fork: None,
            problem: None,
        };
        if options
            .only
            .is_some_and(|wanted| !matches(&library, wanted))
        {
            continue;
        }
        if whose == Whose::Yours {
            let full = options.only.is_some();
            match fork_state(&cache_root, config, &dep, options.fetch, full) {
                Ok(state) => library.fork = Some(state),
                Err(error) => library.problem = Some(error.to_string()),
            }
        } else if let Some(floating) = dep.uses.iter().find(|u| u.pin.floats()) {
            library.tip = tip(git, &mut heads, &dep.url, floating, options.fetch);
        }
        out.push(library);
    }
    heads.save(&cache_root);
    out.sort_by(|a, b| (a.whose, &a.name).cmp(&(b.whose, &b.name)));
    Ok(out)
}

/// Does `wanted` name this library (repository or package, any case)?
pub fn matches(library: &Library, wanted: &str) -> bool {
    library.name.eq_ignore_ascii_case(wanted)
        || library
            .uses
            .iter()
            .any(|u| u.package.eq_ignore_ascii_case(wanted))
}

/// Owners whose forks are yours: configured, or `origin`'s owner.
fn our_owners(git: &Git, config: Option<&Config>) -> Vec<String> {
    if let Some(config) = config {
        if !config.deps.ours.is_empty() {
            return config.deps.ours.clone();
        }
    }
    git.ok(&["remote", "get-url", "origin"])
        .ok()
        .and_then(|url| parts(&normalize(&url)).map(|(_, owner, _)| owner.to_owned()))
        .into_iter()
        .collect()
}

fn cache_root(git: &Git) -> Result<PathBuf> {
    let common = PathBuf::from(git.ok(&["rev-parse", "--git-common-dir"])?);
    let common = if common.is_absolute() {
        common
    } else {
        git.dir().join(common)
    };
    Ok(common.join("tether").join("deps"))
}

/// Branch tips seen by `git ls-remote`, kept for runs without network.
#[derive(Default, Serialize, Deserialize)]
struct Heads(BTreeMap<String, String>);

impl Heads {
    fn file(root: &Path) -> PathBuf {
        root.join("heads.json")
    }
    fn load(root: &Path) -> Heads {
        std::fs::read_to_string(Self::file(root))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }
    fn save(&self, root: &Path) {
        if std::fs::create_dir_all(root).is_ok() {
            let _ = std::fs::write(
                Self::file(root),
                serde_json::to_string_pretty(self).unwrap_or_default(),
            );
        }
    }
}

/// Where a floating pin's branch is now, against what the lockfile builds.
fn tip(git: &Git, heads: &mut Heads, url: &str, using: &Use, fetch: bool) -> Option<Tip> {
    let references: Vec<String> = match &using.pin {
        Pin::Branch(branch) => vec![format!("refs/heads/{branch}")],
        Pin::Ref(reference) => vec![
            format!("refs/heads/{reference}"),
            format!("refs/tags/{reference}"),
        ],
        Pin::Default => vec!["HEAD".to_owned()],
        _ => return None,
    };
    if url.starts_with('-') {
        return None;
    }
    for reference in references {
        let key = format!("{} {reference}", normalize(url));
        if fetch {
            if let Ok(out) = git.ok(&["ls-remote", url, &reference]) {
                if let Some(sha) = out.split_whitespace().next() {
                    heads.0.insert(key.clone(), sha.to_owned());
                }
            }
        }
        if let Some(sha) = heads.0.get(&key) {
            return Some(Tip {
                moved: using.locked.as_deref().is_some_and(|locked| locked != sha),
                reference,
                tip: sha.clone(),
            });
        }
    }
    None
}

/// Find the project a fork came from: configured, or from GitHub.
fn upstream_of(config: Option<&Config>, key: &str) -> Option<(String, Option<String>)> {
    if let Some(entry) = config
        .into_iter()
        .flat_map(|c| &c.deps.forks)
        .find(|f| normalize(&f.url) == key)
    {
        return Some((entry.upstream.clone(), entry.branch.clone()));
    }
    let (host, owner, repo) = parts(key)?;
    if host != "github.com" {
        return None;
    }
    let out = std::process::Command::new("gh")
        .args([
            "api",
            &format!("repos/{owner}/{repo}"),
            "--jq",
            ".parent.clone_url // empty",
        ])
        .env("GH_PROMPT_DISABLED", "1")
        .output()
        .ok()?;
    let parent = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !parent.is_empty()).then_some((parent, None))
}

fn fork_state(
    cache_root: &Path,
    config: Option<&Config>,
    dep: &Found,
    fetch: bool,
    full: bool,
) -> Result<ForkState> {
    let key = normalize(&dep.url);
    if dep.url.starts_with('-') {
        return Err(Error::Refused(format!(
            "not a repository address: {}",
            dep.url
        )));
    }
    let slug: String = key
        .trim_start_matches("https://")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let dir = cache_root.join(format!("{slug}.git"));
    if !dir.exists() {
        if !fetch {
            return Err(Error::Refused(
                "not fetched yet; run `tether deps` with the network".into(),
            ));
        }
        std::fs::create_dir_all(&dir)?;
        Git::new(&dir).ok(&["init", "--bare", "--quiet"])?;
    }
    let cache = Git::new(&dir);
    let using = dep
        .uses
        .iter()
        .find(|u| matches!(u.pin, Pin::Rev(_)))
        .or_else(|| dep.uses.first())
        .expect("a library has a use");
    if fetch {
        let (upstream, branch) = upstream_of(config, &key).ok_or_else(|| {
            Error::Refused(format!(
                "cannot tell which project {key} was forked from; add [[deps.fork]] url = \"{}\" upstream = \"<its upstream>\" to .tether/config.toml",
                dep.url
            ))
        })?;
        set_remote(&cache, "fork", &dep.url)?;
        set_remote(&cache, "upstream", &upstream)?;
        cache.ok(&[
            "fetch",
            "--quiet",
            "--filter=blob:none",
            "--no-tags",
            "fork",
            "+refs/heads/*:refs/remotes/fork/*",
        ])?;
        if let Pin::Rev(rev) = &using.pin {
            if cache.commit_sha(rev)?.is_none() {
                let _ = cache.run(&["fetch", "--quiet", "--filter=blob:none", "fork", rev]);
            }
        }
        let branch = match branch {
            Some(branch) => branch,
            None => default_branch(&cache, "upstream")?,
        };
        cache.ok(&[
            "fetch",
            "--quiet",
            "--filter=blob:none",
            "--no-tags",
            "upstream",
            &format!("+refs/heads/{branch}:refs/remotes/upstream/{branch}"),
            "+refs/tags/*:refs/upstream-tags/*",
        ])?;
        cache.ok(&["config", "tether.branch", &branch])?;
    }
    let branch = cache.ok(&["config", "tether.branch"]).map_err(|_| {
        Error::Refused("not fetched yet; run `tether deps` with the network".into())
    })?;
    let upstream = cache.ok(&["remote", "get-url", "upstream"])?;
    let target = format!("refs/remotes/upstream/{branch}");
    let pinned = pinned_commit(&cache, using)?;
    let base = cache
        .ok(&["merge-base", &pinned, &target])
        .map_err(|_| Error::Refused(format!("{key} shares no history with {upstream}")))?;
    let count = |range: String| -> Result<usize> {
        Ok(cache
            .ok(&["rev-list", "--count", &range])?
            .parse()
            .unwrap_or(0))
    };
    let ahead = count(format!("{target}..{pinned}"))?;
    let behind = count(format!("{pinned}..{target}"))?;
    let yours: BTreeSet<String> = cache
        .raw(&["diff", "--name-only", &base, &pinned])?
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    let keywords = config
        .map(|c| c.security.keywords.clone())
        .unwrap_or_else(|| crate::config::Security::default().keywords);
    let mut changes = upstream_changes(&cache, &format!("{pinned}..{target}"), &yours, &keywords)?;
    let security = changes.iter().filter(|c| !c.security.is_empty()).count();
    let near_yours = changes.iter().filter(|c| !c.near_yours.is_empty()).count();
    if !full {
        changes.retain(|c| !c.security.is_empty() || !c.near_yours.is_empty());
    }
    // For one library: your fork's own commits and every file they change.
    let (patches, files) = if full {
        let patches = upstream_changes(&cache, &format!("{target}..{pinned}"), &yours, &[])?
            .into_iter()
            .map(|c| Patch {
                files: c.near_yours.len(),
                sha: c.sha,
                subject: c.subject,
                timestamp: c.timestamp,
            })
            .collect();
        (patches, yours.iter().cloned().collect())
    } else {
        (Vec::new(), Vec::new())
    };
    Ok(ForkState {
        patches,
        files,
        releases: releases(&cache, &pinned)?,
        conflicts: conflicts(&cache, &pinned, &target),
        newer_on: newer_on(&cache, &pinned)?,
        upstream,
        upstream_branch: branch,
        pinned,
        ahead,
        behind,
        your_files: yours.len(),
        changes,
        security,
        near_yours,
    })
}

fn set_remote(cache: &Git, name: &str, url: &str) -> Result<()> {
    if cache.run(&["remote", "get-url", name])?.success() {
        cache.ok(&["remote", "set-url", name, url])?;
    } else {
        cache.ok(&["remote", "add", name, url])?;
    }
    Ok(())
}

fn default_branch(cache: &Git, remote: &str) -> Result<String> {
    let out = cache.ok(&["ls-remote", "--symref", remote, "HEAD"])?;
    out.lines()
        .find_map(|line| {
            line.strip_prefix("ref: refs/heads/")
                .and_then(|rest| rest.split_whitespace().next())
                .map(str::to_owned)
        })
        .ok_or_else(|| Error::Refused(format!("cannot find the default branch of `{remote}`")))
}

/// The commit the project builds from this fork.
fn pinned_commit(cache: &Git, using: &Use) -> Result<String> {
    let candidates: Vec<String> = match &using.pin {
        Pin::Rev(rev) => vec![rev.clone()],
        Pin::Tag(tag) => vec![format!("refs/tags/{tag}")],
        Pin::Branch(branch) | Pin::Ref(branch) => using
            .locked
            .iter()
            .cloned()
            .chain([format!("refs/remotes/fork/{branch}")])
            .collect(),
        Pin::Default => using.locked.iter().cloned().collect(),
    };
    for candidate in &candidates {
        if let Some(sha) = cache.commit_sha(candidate)? {
            return Ok(sha);
        }
    }
    Err(Error::Refused(format!(
        "the pinned commit ({}) is not in the fork",
        using.pin.text()
    )))
}

fn upstream_changes(
    cache: &Git,
    range: &str,
    yours: &BTreeSet<String>,
    keywords: &[String],
) -> Result<Vec<Change>> {
    let log = cache.raw(&[
        "log",
        "--name-only",
        "--format=%x1e%H%x1f%ct%x1f%s%x1f%b%x1f",
        range,
    ])?;
    let mut out = Vec::new();
    for record in log.split('\x1e').filter(|r| !r.trim().is_empty()) {
        let mut fields = record.splitn(5, '\x1f');
        let (Some(sha), Some(time), Some(subject), Some(body)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let files = fields.next().unwrap_or("");
        out.push(Change {
            sha: sha.trim().to_owned(),
            subject: subject.to_owned(),
            timestamp: time.parse().unwrap_or(0),
            near_yours: files
                .lines()
                .map(str::trim)
                .filter(|f| yours.contains(*f))
                .map(str::to_owned)
                .collect(),
            security: crate::inbox::security_signals(subject, body, keywords),
        });
    }
    Ok(out)
}

/// Upstream releases the pinned commit does not contain, newest first (five).
fn releases(cache: &Git, pinned: &str) -> Result<Vec<String>> {
    let refs = cache.raw(&[
        "for-each-ref",
        "--sort=-v:refname",
        "--format=%(refname:strip=2)%00%(objectname)%00%(*objectname)",
        "refs/upstream-tags",
    ])?;
    let mut out = Vec::new();
    for line in refs.lines() {
        let mut fields = line.split('\0');
        let (Some(name), Some(object)) = (fields.next(), fields.next()) else {
            continue;
        };
        let commit = fields.next().filter(|p| !p.is_empty()).unwrap_or(object);
        if name.contains('-') || cache.is_ancestor(commit, pinned)? {
            continue;
        }
        out.push(name.to_owned());
        if out.len() == 5 {
            break;
        }
    }
    Ok(out)
}

/// Files a merge of upstream into the pinned fork would conflict on; `None`
/// when the preview cannot run (for example, offline before blobs arrived).
fn conflicts(cache: &Git, pinned: &str, target: &str) -> Option<Vec<String>> {
    let out = cache
        .run(&["merge-tree", "--write-tree", "--name-only", pinned, target])
        .ok()?;
    if out.status > 1 {
        return None;
    }
    Some(
        out.stdout
            .lines()
            .skip(1)
            .take_while(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// The fork branch with the most commits past the pin, if any.
fn newer_on(cache: &Git, pinned: &str) -> Result<Option<Newer>> {
    let branches = cache.raw(&[
        "for-each-ref",
        "--format=%(refname:strip=3)",
        "--contains",
        pinned,
        "refs/remotes/fork",
    ])?;
    let mut best: Option<Newer> = None;
    for branch in branches.lines().filter(|b| !b.is_empty() && *b != "HEAD") {
        let commits: usize = cache
            .ok(&[
                "rev-list",
                "--count",
                &format!("{pinned}..refs/remotes/fork/{branch}"),
            ])?
            .parse()
            .unwrap_or(0);
        if commits > 0 && best.as_ref().is_none_or(|b| commits > b.commits) {
            best = Some(Newer {
                branch: branch.to_owned(),
                commits,
            });
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_compare_however_they_are_written() {
        let key = "https://github.com/punit-dethe/xn";
        for url in [
            "https://github.com/Punit-Dethe/xn.git",
            "git+https://github.com/Punit-Dethe/xn",
            "git@github.com:Punit-Dethe/xn.git",
            "ssh://git@github.com/Punit-Dethe/xn/",
        ] {
            assert_eq!(normalize(url), key, "{url}");
        }
        assert_eq!(parts(key), Some(("github.com", "punit-dethe", "xn")));
    }

    #[test]
    fn npm_git_specs() {
        assert_eq!(
            npm_git("github:o/r#v1.2"),
            Some(("https://github.com/o/r".into(), Pin::Ref("v1.2".into())))
        );
        assert_eq!(
            npm_git("o/r#0123abc"),
            Some(("https://github.com/o/r".into(), Pin::Rev("0123abc".into())))
        );
        assert_eq!(
            npm_git("git+https://example.com/o/r.git"),
            Some(("git+https://example.com/o/r.git".into(), Pin::Default))
        );
        for registry in ["^1.2.3", "@scope/pkg", "file:../x", "workspace:*", "latest"] {
            assert_eq!(npm_git(registry), None, "{registry}");
        }
    }

    #[test]
    fn cargo_pins_and_locks() {
        let manifest = r#"
[dependencies]
serde = "1"
a = { git = "https://github.com/o/a", rev = "c3bee28c1d446d95f08c95c3b6f8d4bde052b876" }
[target.'cfg(windows)'.dependencies]
b = { git = "https://github.com/o/b", branch = "dev" }
[patch.crates-io]
c-sys = { git = "https://github.com/o/c.git", package = "c-sys" }
"#;
        let lock = parse_cargo_lock(
            r#"
[[package]]
name = "b"
version = "0.1.0"
source = "git+https://github.com/o/b?branch=dev#1111111111111111111111111111111111111111"
"#,
        );
        let uses = cargo_uses("app/Cargo.toml", manifest, &lock);
        let pins: Vec<(&str, &Pin, Option<&str>)> = uses
            .iter()
            .map(|(_, u)| (u.package.as_str(), &u.pin, u.locked.as_deref()))
            .collect();
        assert_eq!(pins.len(), 3);
        assert!(pins.contains(&(
            "a",
            &Pin::Rev("c3bee28c1d446d95f08c95c3b6f8d4bde052b876".into()),
            None
        )));
        assert!(pins.contains(&(
            "b",
            &Pin::Branch("dev".into()),
            Some("1111111111111111111111111111111111111111")
        )));
        assert!(pins.contains(&("c-sys", &Pin::Default, None)));
    }

    #[test]
    fn submodules_pin_their_recorded_commit() {
        let text =
            "[submodule \"vendor/x\"]\n\tpath = vendor/x\n\turl = https://github.com/o/x.git\n";
        let uses = submodule_uses(text, &|_| Some("ab".repeat(20)));
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].1.pin, Pin::Rev("ab".repeat(20)));
        assert_eq!(uses[0].1.package, "vendor/x");
    }
}
