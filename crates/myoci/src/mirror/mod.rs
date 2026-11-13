#![forbid(unsafe_code)]

pub mod acquire;
pub mod cache;
pub mod graph;
pub mod http;
pub mod inventory;
pub mod json;
pub mod lock;
pub mod ownership;
pub mod provenance;
mod plan;
pub mod reference;
pub mod resolve;
pub mod sync;
pub mod transaction;

pub use inventory::{Inventory, InventoryEntry, PlatformRequirement, ReferrersMode};
pub use lock::{LockedEntry, LockedReferrer, MirrorLock};
pub use reference::RegistryReference;

use crate::error::{Error, Result};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorCommand {
    Resolve,
    Plan,
    Sync,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorOptions {
    pub command: MirrorCommand,
    pub config: String,
    pub lock: String,
    pub state_dir: Option<String>,
    pub locked: bool,
    pub json: bool,
    pub offline: bool,
    pub entries: Vec<String>,
}

pub fn run(argv: &[String]) -> Result<()> {
    let options = parse(argv)?;
    match options.command {
        MirrorCommand::Resolve => resolve::run(&options),
        MirrorCommand::Plan => plan::run(&options),
        MirrorCommand::Sync => sync::run(&options),
    }
}

fn parse(argv: &[String]) -> Result<MirrorOptions> {
    let Some(subcommand) = argv.first() else {
        return Err(Error::Usage(
            "mirror requires a subcommand: resolve, plan, or sync".to_string(),
        ));
    };
    let command = match subcommand.as_str() {
        "resolve" => MirrorCommand::Resolve,
        "plan" => MirrorCommand::Plan,
        "sync" => MirrorCommand::Sync,
        other => {
            return Err(Error::Usage(format!(
                "unknown mirror subcommand {other:?}; expected resolve, plan, or sync"
            )))
        }
    };
    let mut config = None;
    let mut lock = None;
    let mut state_dir = None;
    let mut locked = false;
    let mut json = false;
    let mut offline = false;
    let mut entries = Vec::new();
    let mut index = 1;
    while index < argv.len() {
        match argv[index].as_str() {
            "--config" => config = Some(take(argv, &mut index, "--config")?),
            "--lock" => lock = Some(take(argv, &mut index, "--lock")?),
            "--state-dir" => state_dir = Some(take(argv, &mut index, "--state-dir")?),
            "--locked" => locked = true,
            "--json" => json = true,
            "--offline" => offline = true,
            "--entry" => entries.push(take(argv, &mut index, "--entry")?),
            other => return Err(Error::Usage(format!("unknown option {other}"))),
        }
        index += 1;
    }
    let config = config.ok_or_else(|| Error::Usage("mirror requires --config".to_string()))?;
    let lock = lock.ok_or_else(|| Error::Usage("mirror requires --lock".to_string()))?;
    match command {
        MirrorCommand::Resolve => {
            reject(state_dir.is_some(), "--state-dir is only valid for plan and sync")?;
            reject(locked, "--locked is only valid for sync")?;
            reject(json, "--json is only valid for plan and sync")?;
            reject(offline, "--offline is only valid for sync")?;
        }
        MirrorCommand::Plan => {
            if state_dir.is_none() {
                return Err(Error::Usage("mirror plan requires --state-dir".to_string()));
            }
            reject(locked, "--locked is only valid for sync")?;
            reject(offline, "--offline is only valid for sync")?;
            reject(!entries.is_empty(), "--entry is only valid for resolve")?;
        }
        MirrorCommand::Sync => {
            if state_dir.is_none() {
                return Err(Error::Usage("mirror sync requires --state-dir".to_string()));
            }
            if !locked {
                return Err(Error::Usage("mirror sync requires --locked".to_string()));
            }
            reject(!entries.is_empty(), "--entry is only valid for resolve")?;
        }
    }
    Ok(MirrorOptions {
        command,
        config,
        lock,
        state_dir,
        locked,
        json,
        offline,
        entries,
    })
}

fn reject(present: bool, message: &str) -> Result<()> {
    if present {
        return Err(Error::Usage(message.to_string()));
    }
    Ok(())
}

fn take(argv: &[String], index: &mut usize, flag: &str) -> Result<String> {
    *index += 1;
    argv.get(*index)
        .cloned()
        .ok_or_else(|| Error::Usage(format!("{flag} requires a value")))
}

pub fn print_mirror_help() {
    println!(
        "myoci mirror — reproducible OCI mirroring\n\
         \n\
         usage:\n\
         \x20 myoci mirror resolve --config FILE --lock FILE [--entry NAME]...\n\
         \x20 myoci mirror plan    --config FILE --lock FILE --state-dir DIR [--json]\n\
         \x20 myoci mirror sync    --config FILE --lock FILE --state-dir DIR --locked [--json] [--offline]\n\
         \n\
         resolve fetches mutable tags, verifies the pinned graphs, and rewrites the lock.\n\
         plan compares the inventory, lock, and state without writing or network access.\n\
         sync acquires exactly what the lock records; --offline replays from local state."
    );
}

pub fn state_dir_path(options: &MirrorOptions) -> PathBuf {
    PathBuf::from(options.state_dir.as_deref().expect("validated"))
}

pub fn print_help() {
    print_mirror_help();
}
