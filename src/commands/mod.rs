//! What each command does. Every command returns a [`Report`]; none prints.

mod check;
mod closeout;
mod decide;
mod deps;
mod doctor;
mod folder;
mod inbox;
mod init;
mod map;
mod packet;
mod plan;
mod prove;
mod status;
pub(crate) mod sync;
mod verify;

use std::path::{Path, PathBuf};

use crate::cli::Command;
use crate::config::Config;
use crate::error::Result;
use crate::git::Git;
use crate::report::Report;

pub fn run(command: &Command, dir: &Path) -> Report {
    let name = command.name();
    let result = match command {
        Command::Doctor => doctor::run(dir),
        Command::Prove { from } => prove::run(dir, from.as_deref()),
        Command::Deps { name, no_fetch } => deps::run(dir, name.as_deref(), *no_fetch),
        Command::Ui { port, no_open } => crate::ui::serve(dir, *port, !*no_open)
            .map(|()| Report::success(name, "tether ui stopped.", serde_json::Value::Null))
            .map_err(Into::into),
        // Over MCP a tool cannot start another server.
        Command::Mcp { .. } => Err(crate::error::Error::Refused(
            "`tether mcp` is started by an agent's app, not run as a command".into(),
        )),
        Command::Status => status::run(dir),
        Command::Plan => plan::run(dir),
        Command::Folder {
            name,
            items,
            unfile,
            rename,
            delete,
        } => folder::run(
            dir,
            folder::Args {
                name: name.as_deref(),
                items,
                unfile,
                rename: rename.as_deref(),
                delete: *delete,
            },
        ),
        Command::Init {
            sync_point,
            remote,
            prefix,
        } => init::run(dir, remote, sync_point.as_deref(), prefix.as_deref()),
        Command::Inbox { all, security } => inbox::run(dir, *all, *security),
        Command::Sync {
            target,
            dry_run,
            continue_,
            abort,
            no_fetch,
            skip_verify,
        } => sync::run(
            dir,
            sync::Args {
                target: target.as_deref(),
                dry_run: *dry_run,
                continue_: *continue_,
                abort: *abort,
                no_fetch: *no_fetch,
                skip_verify: *skip_verify,
            },
        ),
        Command::Closeout {
            rev,
            execute,
            no_fetch,
        } => closeout::run(dir, rev, *execute, *no_fetch),
        Command::Check {
            update,
            accept_growth,
            since,
        } => check::run(
            dir,
            check::Args {
                update: *update,
                accept_growth: *accept_growth,
                since: since.as_deref(),
            },
        ),
        Command::Packet { item } => packet::run(dir, item),
        Command::Decide {
            item,
            outcome,
            note,
            port,
            review,
            problem,
            evidence,
            destinations,
            clear,
        } => decide::run(
            dir,
            decide::Args {
                item,
                outcome: outcome.as_deref(),
                note: note.as_deref(),
                port: port.as_deref(),
                review: review.as_deref(),
                problem: problem.as_deref(),
                evidence: evidence.as_deref(),
                destinations,
                clear: *clear,
            },
        ),
        Command::Verify {
            baseline,
            upstream,
            result,
            force,
        } => verify::run(
            dir,
            verify::Args {
                baseline: *baseline,
                upstream: *upstream,
                result: *result,
                force: *force,
            },
        ),
        Command::Map { write, reason, why } => map::run(
            dir,
            map::Args {
                write: *write,
                reason: reason.as_deref(),
                why: why.as_deref(),
            },
        ),
    };
    let mut report = result.unwrap_or_else(|error| Report::failure(name, &error));
    // On an extra upstream, every suggested command must stay on it.
    if let (Some(upstream), Some(next)) = (crate::config::selected_upstream(), &mut report.next) {
        let on = |command: &mut String| {
            if let Some(rest) = command.strip_prefix("tether ") {
                *command = format!("tether -u {upstream} {rest}");
            }
        };
        on(&mut next.command);
        if let Some(packet) = &mut next.packet {
            on(packet);
        }
    }
    report
}

/// The repository root, git bound to it, and its loaded config.
pub(crate) struct Fork {
    pub root: PathBuf,
    pub git: Git,
    pub config: Config,
}

impl Fork {
    pub fn open(dir: &Path) -> Result<Fork> {
        let root = Git::new(dir).toplevel()?;
        let config = Config::load(&root)?;
        Ok(Fork {
            git: Git::new(&root),
            root,
            config,
        })
    }
}
