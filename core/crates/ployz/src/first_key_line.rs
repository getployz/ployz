//! Arming the First Key an install line or `ployz key add` carries: the key a fresh Server
//! trusts to found or join its Cluster, written beside the data directory.

use std::fs::OpenOptions;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;

use ployz_core::first_key::{Armed, FirstKey, FirstKeyError, FirstKeyFile, LineKey};
use ployz_core::{Hint, RpcErrorCode};

use crate::failure::Failure;

const TTY: &str = "/dev/tty";

/// Parse the key an install line carries.
///
/// # Errors
/// Refuses text that isn't a key line Ployz printed.
pub(crate) fn parse(line: &str) -> Result<LineKey, Failure> {
    LineKey::parse(line).map_err(|_| {
        Failure::usage("A key starts with pk1_. Copy it from the install command again.")
    })
}

/// Arm `line` as the First Key of the Server whose data directory is `data_dir`, as the
/// install line does. A disk with no First Key takes it, and one already waiting for it
/// stays as it is. A disk that already enrolled keeps its own keys, so a spent key there is
/// left alone. Replacing anything else asks at this Server's terminal.
///
/// # Errors
/// Refuses when the file can't be read or written, or a replacement isn't confirmed.
pub(crate) fn arm(data_dir: &Path, line: &LineKey) -> Result<(), Failure> {
    let file = FirstKeyFile::beside(data_dir);
    let held = match file
        .lock()
        .and_then(|held| held.arm(line))
        .map_err(failed)?
    {
        Armed::Created => {
            crate::ui::note(will_trust(line));
            return Ok(());
        }
        Armed::AlreadyWaiting => return Ok(()),
        Armed::Held(held) => held,
    };
    if matches!(held, FirstKey::Spent { .. }) && enrolled(data_dir)? {
        return Ok(());
    }
    replace(&file, data_dir, &held, line)?;
    crate::ui::note(will_trust(line));
    Ok(())
}

/// Re-arm `line` on a disk that hasn't enrolled, as `ployz key add` does there: a disk with
/// no First Key takes it, and replacing one asks at this Server's terminal. The caller
/// reports it.
///
/// # Errors
/// As [`arm`].
pub(crate) fn rearm(data_dir: &Path, line: &LineKey) -> Result<(), Failure> {
    let file = FirstKeyFile::beside(data_dir);
    match file
        .lock()
        .and_then(|held| held.arm(line))
        .map_err(failed)?
    {
        Armed::Created | Armed::AlreadyWaiting => Ok(()),
        Armed::Held(held) => replace(&file, data_dir, &held, line),
    }
}

/// What a Server whose First Key is `line` will do with it.
pub(crate) fn will_trust(line: &LineKey) -> String {
    format!(
        "This Server will trust {} when it joins or founds a Cluster.",
        line.describe()
    )
}

/// Whether the disk at `data_dir` has enrolled in a Cluster.
///
/// # Errors
/// Refuses when its record can't be read.
pub(crate) fn enrolled(data_dir: &Path) -> Result<bool, Failure> {
    ployz_core::first_key::disk_enrolled(data_dir).map_err(|error| {
        Failure::caused(
            RpcErrorCode::Internal,
            format!(
                "Could not read this Server's record in {}.",
                data_dir.display()
            ),
            error,
        )
    })
}

/// Ask at the terminal, with the lock let go, then replace `held` with `line` only if the
/// file still holds it and the disk still hasn't enrolled.
fn replace(
    file: &FirstKeyFile,
    data_dir: &Path,
    held: &FirstKey,
    line: &LineKey,
) -> Result<(), Failure> {
    confirm(held, line)?;
    let locked = file.lock().map_err(failed)?;
    if locked.read().map_err(failed)?.as_ref() != Some(held) || enrolled(data_dir)? {
        return Err(Failure::conflict(
            "This Server's First Key changed while you were confirming. Nothing was replaced. Run the command again.",
        ));
    }
    locked.rearm(line).map_err(failed)
}

/// Show what the line would replace and ask for the new key's code at this Server's own
/// terminal. Never `--yes` or the environment: whoever wrote the line must not answer it.
/// A run that writes JSON is read by a program, so it never asks.
fn confirm(held: &FirstKey, line: &LineKey) -> Result<(), Failure> {
    let code = line.code().to_string();
    let refused = |why: &str| {
        Failure::conflict(format!("{} {why}", holds(held))).hint(Hint::Next(format!(
            "sudo ployz key add {line} at a terminal on this Server"
        )))
    };
    if crate::ui::json() {
        return Err(refused(
            "--json can't ask you to confirm the new key, so nothing changed.",
        ));
    }
    let no_terminal = || refused("No terminal here to confirm the new key, so nothing changed.");
    let Ok(tty) = OpenOptions::new().read(true).write(true).open(TTY) else {
        return Err(no_terminal());
    };
    let mut out = &tty;
    write!(
        out,
        "{}\nThis Server would trust {} to join or found a Cluster instead. Trust it only if you copied this command yourself.\nType {code} to trust it: ",
        holds(held),
        line.describe(),
    )
    .map_err(|_| no_terminal())?;
    let mut typed = String::new();
    BufReader::new(&tty)
        .read_line(&mut typed)
        .map_err(|_| no_terminal())?;
    if typed.trim() == code {
        Ok(())
    } else {
        Err(
            Failure::conflict(format!("That isn't {code}, so nothing changed."))
                .hint(Hint::Next(format!("sudo ployz key add {line}"))),
        )
    }
}

fn holds(held: &FirstKey) -> String {
    match held {
        FirstKey::Waiting { key } => format!("This Server waits to trust {}.", key.describe()),
        FirstKey::Spent { .. } => {
            "This Server already used its First Key, so an install command can't set a new one."
                .to_owned()
        }
    }
}

fn failed(error: FirstKeyError) -> Failure {
    Failure::caused(
        RpcErrorCode::Internal,
        "Could not use this Server's First Key file.",
        error,
    )
}
