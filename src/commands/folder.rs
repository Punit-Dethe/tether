//! `tether folder`: group upstream commits into folders of your own.
//!
//!   folder                          list folders
//!   folder <name> [<item>…]         file commits into a folder (creates it)
//!   folder --unfile <item>…         take commits out of their folder
//!   folder <name> --rename <new>    rename a folder
//!   folder <name> --delete          delete a folder (its commits stay)

use std::path::Path;

use serde_json::json;

use super::Fork;
use crate::error::{Error, Result};
use crate::folders;
use crate::inbox;
use crate::report::Report;

pub struct Args<'a> {
    pub name: Option<&'a str>,
    pub items: &'a [String],
    pub unfile: &'a [String],
    pub rename: Option<&'a str>,
    pub delete: bool,
}

pub fn run(dir: &Path, args: Args) -> Result<Report> {
    let fork = Fork::open(dir)?;
    let mut all = folders::load(&fork.root)?;
    let resolve = |refs: &[String]| -> Result<Vec<String>> {
        if refs.is_empty() {
            return Ok(Vec::new());
        }
        let inbox = inbox::build(&fork.git, &fork.root, &fork.config)?;
        refs.iter()
            .map(|r| inbox::resolve(&inbox.items, r).map(|item| item.sha.clone()))
            .collect()
    };

    let summary = match (args.name, args.rename, args.delete) {
        (None, _, _) if !args.unfile.is_empty() => {
            let shas = resolve(args.unfile)?;
            all.unfile(&shas);
            format!("Took {} commit(s) out of their folder.", shas.len())
        }
        (None, None, false) => {
            let data = json!({ "folders": listing(&all) });
            let summary = match all.folders.len() {
                0 => "No folders yet; `tether folder <name> <item>…` makes one.".to_owned(),
                n => format!("{n} folder(s)."),
            };
            return Ok(Report::success("folder", summary, data));
        }
        (None, _, _) => {
            return Err(Error::Refused(
                "name the folder: `tether folder <name> --rename <new>` or `--delete`".into(),
            ));
        }
        (Some(name), Some(new), false) => {
            all.rename(name, new)?;
            format!("Renamed `{name}` to `{new}`.")
        }
        (Some(name), None, true) => {
            all.delete(name)?;
            format!("Deleted folder `{name}`; its commits are back in the inbox.")
        }
        (Some(_), Some(_), true) => {
            return Err(Error::Refused(
                "--rename and --delete are separate steps".into(),
            ));
        }
        (Some(name), None, false) => {
            let shas = resolve(args.items)?;
            all.file(name, &shas)?;
            format!("Filed {} commit(s) into `{}`.", shas.len(), name.trim())
        }
    };
    folders::save(&fork.root, &all)?;
    Ok(Report::success(
        "folder",
        summary,
        json!({ "folders": listing(&all) }),
    ))
}

fn listing(all: &folders::Folders) -> serde_json::Value {
    all.folders
        .iter()
        .map(|folder| json!({ "name": folder.name, "count": folder.items.len() }))
        .collect()
}
