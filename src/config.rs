//! `.tether/config.toml`: everything tether needs to know about one fork.
//!
//! Only `[upstream]` is required. Every other table describes a way the fork
//! departs from upstream, and is empty for a fork that never moved anything.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths::glob_match;

pub const STATE_DIR: &str = ".tether";
pub const CONFIG_FILE: &str = "config.toml";
/// Where each extra upstream keeps its own config, decisions and budgets.
pub const UPSTREAMS_DIR: &str = "upstreams";

/// The extra upstream this process works on (`--upstream <name>`); `None`
/// is the main one. Set once at start, or per call by the MCP server.
static SELECTED: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// Work on the extra upstream `name` (or the main one, for `None`).
pub fn select_upstream(name: Option<&str>) -> Result<()> {
    if let Some(name) = name {
        let valid = !name.is_empty()
            && name.len() <= 40
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
        if !valid {
            return Err(Error::Refused(format!(
                "an upstream name is lower-case letters, digits, `-` or `_`: {name:?}"
            )));
        }
    }
    *SELECTED.write().expect("upstream selection lock") = name.map(str::to_owned);
    Ok(())
}

/// The extra upstream selected, if any.
pub fn selected_upstream() -> Option<String> {
    SELECTED.read().expect("upstream selection lock").clone()
}

/// The selected upstream's state directory, relative to the repository:
/// `.tether`, or `.tether/upstreams/<name>`.
pub fn state_dir() -> String {
    match selected_upstream() {
        Some(name) => format!("{STATE_DIR}/{UPSTREAMS_DIR}/{name}"),
        None => STATE_DIR.to_owned(),
    }
}

/// Names of the extra upstreams set up in this repository.
pub fn extra_upstreams(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join(STATE_DIR).join(UPSTREAMS_DIR))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().join(CONFIG_FILE).is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub upstream: Upstream,

    /// Folders the fork moved: upstream's files live under a new prefix.
    #[serde(default, rename = "moved", skip_serializing_if = "Vec::is_empty")]
    pub moved: Vec<Moved>,

    /// Areas the fork owns outright; upstream's changes there never land.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owned: Vec<Owned>,

    /// Areas where every upstream change needs a recorded review.
    #[serde(default, rename = "review", skip_serializing_if = "Vec::is_empty")]
    pub reviews: Vec<ReviewArea>,

    /// Upstream files whose fixes cannot reach the fork by merge alone.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub relocations: BTreeMap<String, Relocation>,

    #[serde(default)]
    pub budget: Budget,

    /// Intent markers and the generated divergence map.
    #[serde(default)]
    pub map: MapConfig,

    /// What marks an upstream commit as a security fix.
    #[serde(default)]
    pub security: Security,

    /// How `tether sync` runs the merge.
    #[serde(default, skip_serializing_if = "SyncConfig::is_empty")]
    pub sync: SyncConfig,

    #[serde(default)]
    pub port_audit: PortAudit,

    #[serde(default)]
    pub agent: Agent,

    /// Build and test commands `tether verify` runs.
    #[serde(default, skip_serializing_if = "Verify::is_empty")]
    pub verify: Verify,

    /// Libraries the fork takes from git rather than a registry (`tether deps`).
    #[serde(default, skip_serializing_if = "Deps::is_empty")]
    pub deps: Deps,

    /// `budget.scope` was filled in from `upstream.prefix`, not written.
    #[serde(skip)]
    pub scope_from_prefix: bool,
}

/// Forked libraries (`tether deps`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deps {
    /// Owners (GitHub users or organisations) whose forks you maintain
    /// (default: the owner of the `origin` remote).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ours: Vec<String>,
    /// The project each of your forks came from, when the host cannot tell.
    #[serde(default, rename = "fork", skip_serializing_if = "Vec::is_empty")]
    pub forks: Vec<DepFork>,
}

impl Deps {
    pub fn is_empty(&self) -> bool {
        self.ours.is_empty() && self.forks.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepFork {
    /// The fork, as the manifest names it.
    pub url: String,
    /// The project it was forked from.
    pub upstream: String,
    /// Upstream's branch to compare with (default: its default branch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// How to build and test the fork (`tether verify`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verify {
    /// Seconds one step may run before it is stopped and counted as failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// The shell a step's `run` line goes to (default `sh -c`, or `cmd /C`
    /// on Windows).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shell: Vec<String>,
    /// Environment variables for every step.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, rename = "step", skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
}

impl Verify {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// One build or test command.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub name: String,
    /// The command line, run from the repository root.
    pub run: String,
    /// How to read individual failures from the output: `exit` (the step
    /// passes or fails as a whole), `cargo`, `pytest`, `go` or `tap`.
    #[serde(default = "default_format")]
    pub format: String,
    /// Count every output line starting with this as one failure (instead
    /// of `format`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// The command for upstream's tree, when it differs; `""` skips this
    /// step on upstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
}

fn default_format() -> String {
    "exit".into()
}

pub const FORMATS: &[&str] = &["exit", "cargo", "pytest", "go", "tap"];

/// How tether works with AI agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    /// Upper bound on a work packet's size, in bytes (~4 bytes per token).
    #[serde(default = "default_packet_bytes")]
    pub packet_bytes: usize,
    /// Actions an agent may not take alone (when `TETHER_ACTOR=agent`).
    /// Patterns: `accept-growth`, `skip-verify`, `sync-unreleased`, `closeout`, `abort`, `merged`, `ignored`,
    /// `pending`, `note`, `clear`, `port:<outcome>`, `review:<area>`;
    /// `*` matches anything.
    #[serde(default = "default_needs_human")]
    pub needs_human: Vec<String>,
}

impl Default for Agent {
    fn default() -> Self {
        Agent {
            packet_bytes: default_packet_bytes(),
            needs_human: default_needs_human(),
        }
    }
}

fn default_packet_bytes() -> usize {
    48_000
}
fn default_needs_human() -> Vec<String> {
    vec![
        "accept-growth".into(),
        "skip-verify".into(),
        "sync-unreleased".into(),
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upstream {
    #[serde(default = "default_remote")]
    pub remote: String,
    #[serde(default = "default_branch")]
    pub branch: String,
    /// Display name, used in sync commit subjects ("sync(upstream): Handy …").
    #[serde(default)]
    pub name: Option<String>,
    /// Web address credited in sync commit messages.
    #[serde(default)]
    pub url: Option<String>,
    /// Track upstream commits from this committer date on (RFC 3339, UTC).
    /// Without it the inbox lists only commits past the sync point.
    #[serde(default)]
    pub since: Option<String>,
    /// Follow upstream's `branch` tip, or its newest `releases` tag.
    #[serde(default, skip_serializing_if = "Track::is_branch")]
    pub track: Track,
    /// Which upstream tags are releases (fnmatch pattern).
    #[serde(default = "default_tags", skip_serializing_if = "is_default_tags")]
    pub tags: String,
    /// Count tags with a pre-release suffix (`v2.0.0-rc.1`) as releases.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub prereleases: bool,
    /// The folder upstream's whole tree lives in, for a project vendored
    /// into a subfolder (`vendor/lib/`). Ends with `/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Track {
    #[default]
    Branch,
    Releases,
}

impl Track {
    pub fn is_branch(&self) -> bool {
        *self == Track::Branch
    }
}

fn default_tags() -> String {
    "v*".into()
}
fn is_default_tags(tags: &str) -> bool {
    tags == "v*"
}

/// How `tether sync` runs the merge.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncConfig {
    /// git settings for the merge only (`-c key=value`), so every clone and
    /// CI runner merges the same way: e.g. `merge.directoryRenames = "true"`,
    /// or `merge.ours.driver = "true"` for `merge=ours` attributes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub git_config: BTreeMap<String, String>,
    /// A folder in the repository that shares rerere's recorded conflict
    /// resolutions between clones: restored before each sync, new ones saved
    /// after it (commit them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerere_cache: Option<String>,
}

impl SyncConfig {
    pub fn is_empty(&self) -> bool {
        self.git_config.is_empty() && self.rerere_cache.is_none()
    }

    /// The `-c key=value` pairs a sync's merge runs with.
    pub fn merge_settings(&self) -> Vec<String> {
        let mut settings: Vec<String> = self
            .git_config
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        if self.rerere_cache.is_some() && !self.git_config.contains_key("rerere.enabled") {
            settings.push("rerere.enabled=true".into());
        }
        // Reused rerere content is review input, never an auto-staged decision.
        settings.push("rerere.autoupdate=false".into());
        settings
    }
}

/// Words in a commit subject that flag a security fix.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Security {
    #[serde(default = "default_security_keywords")]
    pub keywords: Vec<String>,
}

impl Default for Security {
    fn default() -> Self {
        Security {
            keywords: default_security_keywords(),
        }
    }
}

fn default_security_keywords() -> Vec<String> {
    [
        "security",
        "vulnerability",
        "vulnerable",
        "exploit",
        "xss",
        "csrf",
        "ssrf",
        "rce",
        "injection",
        "path traversal",
        "privilege escalation",
        "sandbox escape",
        "denial of service",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn default_remote() -> String {
    "upstream".into()
}
fn default_branch() -> String {
    "main".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Moved {
    /// Where the files live in the fork, e.g. `src-tauri/src/handy/`.
    pub fork: String,
    /// Where the same files live upstream, e.g. `src-tauri/src/`.
    pub upstream: String,
    /// Upstream files under `upstream` that deliberately did not move.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stay: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Owned {
    pub path: String,
    /// Fork code inside `path` whose conflicts still need a person.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub except: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewArea {
    pub name: String,
    /// fnmatch-style patterns; `*` also crosses `/`.
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RelocationKind {
    /// Upstream's file is kept but not used; a fork file replaces it.
    Inert,
    /// Some logic moved out of an otherwise-merged file.
    Relocated,
    /// A fork-only twin shares the bug class.
    Parallel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relocation {
    pub kind: RelocationKind,
    /// Fork files that must carry this upstream file's fixes.
    pub fork: Vec<String>,
    #[serde(default)]
    pub why: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    /// Path prefixes whose upstream-derived files carry a divergence budget.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    /// Files outside the budget (generated files, lock files).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortAudit {
    /// Upstream commits at or before this one may be acknowledged with a
    /// plain note instead of per-file port records.
    #[serde(default)]
    pub notes_accepted_through: Option<String>,
    /// A Markdown file whose table rows queue deferred ports: a `deferred`
    /// port record counts only while its commit's full sha is in the first
    /// column of a row. Without it, `deferred` always counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_queue: Option<String>,
}

/// Intent markers (`tether map`, and the unmarked-change check).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapConfig {
    /// The text that marks a fork change in upstream-derived code, written in
    /// a comment with the reason after it: `// tether: keep the fork's limit`.
    #[serde(default = "default_marker")]
    pub marker: String,
    /// How many lines above a change its marker may sit.
    #[serde(default = "default_window")]
    pub window: usize,
    /// Fork path prefixes whose upstream-derived files need markers
    /// (default: `budget.scope`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    /// Files that never need markers (generated files, data files).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

impl Default for MapConfig {
    fn default() -> Self {
        MapConfig {
            marker: default_marker(),
            window: default_window(),
            scope: Vec::new(),
            exclude: Vec::new(),
        }
    }
}

fn default_marker() -> String {
    "tether:".into()
}
fn default_window() -> usize {
    3
}

impl Config {
    /// Prefixes whose files need intent markers.
    pub fn map_scope(&self) -> &[String] {
        if self.map.scope.is_empty() && self.scope_from_prefix {
            &[]
        } else if self.map.scope.is_empty() {
            &self.budget.scope
        } else {
            &self.map.scope
        }
    }

    /// The selected upstream's config file (see [`select_upstream`]).
    pub fn path(root: &Path) -> PathBuf {
        root.join(state_dir()).join(CONFIG_FILE)
    }

    pub fn exists(root: &Path) -> bool {
        Self::path(root).is_file()
    }

    /// The selected upstream's config, as a repository path for messages.
    pub fn display_path() -> String {
        format!("{}/{CONFIG_FILE}", state_dir())
    }

    /// Is the main upstream set up (whichever upstream is selected)?
    pub fn exists_main(root: &Path) -> bool {
        root.join(STATE_DIR).join(CONFIG_FILE).is_file()
    }

    /// The main upstream's config, whichever upstream is selected.
    pub fn load_main(root: &Path) -> Result<Config> {
        let selected = selected_upstream();
        select_upstream(None)?;
        let config = Self::load(root);
        select_upstream(selected.as_deref())?;
        config
    }

    pub fn load(root: &Path) -> Result<Config> {
        let path = Self::path(root);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotConfigured);
            }
            Err(error) => return Err(error.into()),
        };
        let mut config: Config = toml::from_str(&text).map_err(|error| Error::BadConfig {
            path: format!("{}/{CONFIG_FILE}", state_dir()),
            message: error.to_string(),
        })?;
        config.validate()?;
        // A vendored upstream's divergence is only what lies in its folder.
        if let Some(prefix) = &config.upstream.prefix {
            if config.budget.scope.is_empty() {
                config.budget.scope = vec![prefix.clone()];
                config.scope_from_prefix = true;
            }
        }
        Ok(config)
    }

    /// Extra `git merge` / `merge-tree` options: a vendored upstream's tree
    /// is shifted into its folder.
    pub fn merge_options(&self) -> Vec<String> {
        match &self.upstream.prefix {
            Some(prefix) => vec![format!("-Xsubtree={}", prefix.trim_end_matches('/'))],
            None => Vec::new(),
        }
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("config is always serialisable")
    }

    pub(crate) fn validate(&self) -> Result<()> {
        let bad = |message: String| Error::BadConfig {
            path: format!("{}/{CONFIG_FILE}", state_dir()),
            message,
        };
        if let Some(prefix) = &self.upstream.prefix {
            if !prefix.ends_with('/') || prefix.starts_with('/') || prefix.contains("..") {
                return Err(bad(format!(
                    "upstream.prefix must be a relative folder ending with `/`: {prefix}"
                )));
            }
            if prefix.starts_with(".tether/") {
                return Err(bad("upstream.prefix cannot be inside .tether/".into()));
            }
        }
        if self.map.marker.trim().is_empty() {
            return Err(bad("[map] marker must not be empty".into()));
        }
        let mut names = std::collections::BTreeSet::new();
        for step in &self.verify.steps {
            if !names.insert(step.name.as_str()) {
                return Err(bad(format!(
                    "[[verify.step]] names must be unique: {}",
                    step.name
                )));
            }
            if step.prefix.is_none() && !FORMATS.contains(&step.format.as_str()) {
                return Err(bad(format!(
                    "[[verify.step]] {}: format must be one of {}",
                    step.name,
                    FORMATS.join(", ")
                )));
            }
        }
        for moved in &self.moved {
            if !moved.fork.ends_with('/') || !moved.upstream.ends_with('/') {
                return Err(bad(format!(
                    "[[moved]] prefixes must end with `/`: {} -> {}",
                    moved.upstream, moved.fork
                )));
            }
        }
        for owned in &self.owned {
            if !owned.path.ends_with('/') {
                return Err(bad(format!(
                    "[[owned]] path must end with `/`: {}",
                    owned.path
                )));
            }
        }
        if let Some(since) = &self.upstream.since {
            crate::time::parse_utc(since).ok_or_else(|| {
                bad(format!(
                    "upstream.since is not an RFC 3339 UTC time: {since}"
                ))
            })?;
        }
        Ok(())
    }

    /// Does policy reserve `action` for a person?
    pub fn needs_human(&self, action: &str) -> bool {
        self.agent
            .needs_human
            .iter()
            .any(|pattern| glob_match(pattern, action))
    }

    /// The upstream branch as a local ref, e.g. `upstream/main`.
    pub fn upstream_ref(&self) -> String {
        format!("{}/{}", self.upstream.remote, self.upstream.branch)
    }

    pub fn upstream_name(&self) -> &str {
        self.upstream.name.as_deref().unwrap_or("upstream")
    }

    /// The path the same file has in upstream's tree.
    pub fn to_upstream_path(&self, fork_path: &str) -> String {
        for moved in &self.moved {
            if let Some(rest) = fork_path.strip_prefix(&moved.fork) {
                return format!("{}{rest}", moved.upstream);
            }
        }
        if let Some(prefix) = &self.upstream.prefix {
            if let Some(rest) = fork_path.strip_prefix(prefix.as_str()) {
                return rest.to_owned();
            }
        }
        fork_path.to_owned()
    }

    /// The path an upstream file has in the fork (the reverse of
    /// [`Config::to_upstream_path`]).
    pub fn to_fork_path(&self, upstream_path: &str) -> String {
        for moved in &self.moved {
            if let Some(rest) = upstream_path.strip_prefix(&moved.upstream) {
                if !upstream_path.starts_with(&moved.fork) {
                    return format!("{}{rest}", moved.fork);
                }
            }
        }
        if let Some(prefix) = &self.upstream.prefix {
            return format!("{prefix}{upstream_path}");
        }
        upstream_path.to_owned()
    }

    /// Does upstream lose every conflict at `path`? True when the path, or
    /// where the fork moved it to, is inside an owned area (and not in one of
    /// its exceptions).
    pub fn is_owned(&self, path: &str) -> bool {
        let owned = |path: &str| {
            self.owned.iter().any(|area| {
                path.starts_with(&area.path) && !area.except.iter().any(|e| path.starts_with(e))
            })
        };
        let state = self
            .state_dirs()
            .iter()
            .any(|dir| path.starts_with(dir.as_str()));
        state || owned(path) || owned(&self.to_fork_path(path))
    }

    /// Directories holding tether's own state (`.tether/`, the shared rerere
    /// cache), each ending in `/`. They are never upstream's: a rerere
    /// preimage looks enough like the file it came from for rename detection
    /// to merge upstream's edits into it.
    pub fn state_dirs(&self) -> Vec<String> {
        let mut dirs = vec![".tether/".to_owned()];
        if let Some(cache) = &self.sync.rerere_cache {
            let cache = format!("{}/", cache.trim_end_matches('/'));
            if !cache.starts_with(".tether/") {
                dirs.push(cache);
            }
        }
        dirs
    }

    /// Review areas an upstream change to `path` falls in.
    pub fn review_areas_for<'a>(&'a self, path: &str) -> impl Iterator<Item = &'a str> + 'a {
        let path = path.to_owned();
        self.reviews
            .iter()
            .filter(move |area| area.paths.iter().any(|pattern| glob_match(pattern, &path)))
            .map(|area| area.name.as_str())
    }
}
