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

    #[serde(default)]
    pub port_audit: PortAudit,
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
}

impl Config {
    pub fn path(root: &Path) -> PathBuf {
        root.join(STATE_DIR).join(CONFIG_FILE)
    }

    pub fn exists(root: &Path) -> bool {
        Self::path(root).is_file()
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
        let config: Config = toml::from_str(&text).map_err(|error| Error::BadConfig {
            path: format!("{STATE_DIR}/{CONFIG_FILE}"),
            message: error.to_string(),
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("config is always serialisable")
    }

    fn validate(&self) -> Result<()> {
        let bad = |message: String| Error::BadConfig {
            path: format!("{STATE_DIR}/{CONFIG_FILE}"),
            message,
        };
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
        owned(path) || owned(&self.to_fork_path(path))
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
