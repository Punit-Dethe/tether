//! Folders: a person's own grouping of upstream commits,
//! `.tether/folders.toml`.
//!
//! Purely for reading the inbox: a folder never changes a commit's state or
//! what a sync does. A commit sits in at most one folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::STATE_DIR;
use crate::error::{Error, Result};

pub const FILE: &str = "folders.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Folder {
    pub name: String,
    /// Full commit ids, in the order they were filed.
    #[serde(default)]
    pub items: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Folders {
    #[serde(default, rename = "folder")]
    pub folders: Vec<Folder>,
}

fn path(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join(FILE)
}

pub fn load(root: &Path) -> Result<Folders> {
    let path = path(root);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Folders::default()),
        Err(err) => return Err(err.into()),
    };
    toml::from_str(&text).map_err(|err| Error::BadConfig {
        path: format!("{STATE_DIR}/{FILE}"),
        message: err.message().to_owned(),
    })
}

pub fn save(root: &Path, folders: &Folders) -> Result<()> {
    let path = path(root);
    if folders.folders.is_empty() {
        match std::fs::remove_file(&path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err.into()),
            _ => return Ok(()),
        }
    }
    std::fs::create_dir_all(path.parent().expect("state dir"))?;
    let text = toml::to_string(folders).expect("folders are always serialisable");
    std::fs::write(&path, text)?;
    Ok(())
}

fn check_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() || name.len() > 80 || name.chars().any(char::is_control) {
        return Err(Error::Refused(
            "a folder name must be 1–80 printable characters".into(),
        ));
    }
    Ok(name)
}

impl Folders {
    /// The folder holding `sha`.
    pub fn of(&self, sha: &str) -> Option<&str> {
        self.folders
            .iter()
            .find(|folder| folder.items.iter().any(|item| item == sha))
            .map(|folder| folder.name.as_str())
    }

    fn find(&mut self, name: &str) -> Option<&mut Folder> {
        self.folders
            .iter_mut()
            .find(|folder| folder.name.eq_ignore_ascii_case(name))
    }

    /// Take `shas` out of whatever folder holds them.
    pub fn unfile(&mut self, shas: &[String]) {
        for folder in &mut self.folders {
            folder.items.retain(|item| !shas.contains(item));
        }
    }

    /// File `shas` into `name`, creating it; they leave any other folder.
    pub fn file(&mut self, name: &str, shas: &[String]) -> Result<()> {
        let name = check_name(name)?;
        self.unfile(shas);
        if self.find(name).is_none() {
            self.folders.push(Folder {
                name: name.to_owned(),
                items: Vec::new(),
            });
        }
        let folder = self.find(name).expect("just created");
        folder.items.extend(shas.iter().cloned());
        Ok(())
    }

    pub fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        let to = check_name(to)?;
        if !from.eq_ignore_ascii_case(to) && self.find(to).is_some() {
            return Err(Error::Refused(format!(
                "a folder named `{to}` already exists"
            )));
        }
        let folder = self
            .find(from)
            .ok_or_else(|| Error::Refused(format!("no folder named `{from}`")))?;
        folder.name = to.to_owned();
        Ok(())
    }

    /// Remove the folder; its commits go back to the inbox unfiled.
    pub fn delete(&mut self, name: &str) -> Result<()> {
        let before = self.folders.len();
        self.folders
            .retain(|folder| !folder.name.eq_ignore_ascii_case(name));
        if self.folders.len() == before {
            return Err(Error::Refused(format!("no folder named `{name}`")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shas(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_commit_lives_in_one_folder() {
        let mut folders = Folders::default();
        folders.file("Translations", &shas(&["a", "b"])).unwrap();
        folders.file("CI", &shas(&["b"])).unwrap();
        assert_eq!(folders.of("a"), Some("Translations"));
        assert_eq!(folders.of("b"), Some("CI"));
        folders.file("translations", &shas(&["c"])).unwrap();
        assert_eq!(folders.folders.len(), 2, "names match case-insensitively");
    }

    #[test]
    fn rename_and_delete() {
        let mut folders = Folders::default();
        folders.file("Docs", &shas(&["a"])).unwrap();
        folders.file("CI", &[]).unwrap();
        assert!(folders.rename("Docs", "CI").is_err());
        folders.rename("Docs", "Guides").unwrap();
        assert_eq!(folders.of("a"), Some("Guides"));
        folders.delete("guides").unwrap();
        assert_eq!(folders.of("a"), None);
        assert!(folders.delete("nope").is_err());
        assert!(folders.file("  ", &[]).is_err());
    }
}
