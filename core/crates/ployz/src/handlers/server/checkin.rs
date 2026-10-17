//! `ployz server checkin`: a waiting host checks in with Cloud until it is approved.
//!
//! `--install` writes the loop and preparation units, checks in once, and leaves the loop
//! running under systemd. Without it, this is the loop the unit runs.

use std::{
    env as process_env,
    ffi::OsStr,
    fs,
    io::{self, IsTerminal, Read},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use chrono::Utc;
use clap::{ArgMatches, Command, parser::ValueSource};
use ployz_core::RpcErrorCode;

use super::super::{Error, leaf_matches, runtime};
use crate::checkin::{
    self, CheckinClient, CheckinEnd, CheckinKey, FleetToken, Hold, Next, Paths, StopReason, System,
    unit,
};
use crate::cli::{base, env, switch, value};
use crate::cloud_login::DEFAULT_CLOUD;

const UNIT_DIR: &str = "/etc/systemd/system";
const SKIP_ZFS: &str = "PLOYZ_SKIP_ZFS";
const TOKEN_MODE: u32 = 0o600;

pub(crate) fn command(command: Command) -> Command {
    base(command, "Check in with Cloud until this host is approved")
        .long_about("Check in with Cloud until this host is approved. --install writes the ployz-checkin and ployz-prepare units, checks in once, and leaves the loop running; it ends when Cloud enrolls or rejects the host, or the Fleet Token is revoked or expires. Without --install, this runs the loop in the foreground.")
        .arg(switch("install", None).help("Install the check-in units, check in once, and leave the loop running"))
        .arg(
            value("token-file", None)
                .value_parser(clap::value_parser!(PathBuf))
                .default_value(Paths::TOKEN_FILE)
                .help("Read the Fleet Token from this file"),
        )
        .arg(
            switch("token-stdin", None)
                .requires("install")
                .help("Read the Fleet Token from stdin, and write it to --token-file once Cloud answers it"),
        )
        .arg(
            value("cloud-url", None)
                .env(env::CLOUD_URL)
                .help("Cloud to check in with [default: ployz.dev]"),
        )
        .arg(
            value("ployzd", None)
                .value_parser(clap::value_parser!(PathBuf))
                .hide(true)
                .help("Probe and prepare with this ployzd, the one --install chose"),
        )
}

pub(crate) fn checkin(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let token_file = matches
        .get_one::<PathBuf>("token-file")
        .cloned()
        .unwrap_or_else(|| PathBuf::from(Paths::TOKEN_FILE));
    // The unit runs from /, so a path relative to here would name another file there.
    let token_file = std::path::absolute(&token_file).map_err(|error| {
        Error::caused(
            RpcErrorCode::InvalidArgument,
            "Could not resolve the --token-file path.",
            error,
        )
    })?;
    let cloud_url = matches
        .get_one::<String>("cloud-url")
        .map_or(DEFAULT_CLOUD, String::as_str)
        .to_owned();
    let paths = Paths::system(token_file);
    if !crate::provisioning::process_is_root() {
        return Err(Error::coded(
            RpcErrorCode::Unauthenticated,
            "Run ployz server checkin as root.",
        ));
    }
    let runtime = runtime()?;
    if matches.get_flag("install") {
        let piped = matches.get_flag("token-stdin").then(std::io::stdin);
        let (token_source, key, mut client) = install_start(&paths, &cloud_url, piped)?;
        let url_source = match matches.value_source("cloud-url") {
            Some(ValueSource::CommandLine) => Some("--cloud-url"),
            Some(ValueSource::EnvVariable) => Some(env::CLOUD_URL),
            _ => None,
        };
        let install_with = Install {
            cloud_url: &cloud_url,
            url_source,
            token: token_source,
        };
        return runtime.block_on(install(&mut client, &key, &paths, install_with));
    }
    let Some((ployzd, token)) = runtime.block_on(loop_start(
        &System::with(PathBuf::new()),
        matches,
        &paths,
        process_env::var_os("PATH").as_deref(),
    ))?
    else {
        return Ok(());
    };
    let host = System {
        ployzd: ployzd.path,
        stale: ployzd.stale,
    };
    let (key, mut client) = connect(&paths, &cloud_url, token)?;
    match runtime.block_on(checkin::run(&host, &mut client, &key, &paths)) {
        CheckinEnd::Stopped(_) => Ok(()),
        // A failed exit, so systemd starts the loop again with the token now in the file.
        CheckinEnd::TokenChanged => Err(Error::coded(
            RpcErrorCode::Unavailable,
            "The Fleet Token changed as the check-in loop stopped. It restarts with the new one.",
        )),
    }
}

fn install_start(
    paths: &Paths,
    cloud_url: &str,
    piped: Option<impl Read + IsTerminal>,
) -> Result<(Token, CheckinKey, CheckinClient), Error> {
    let (token, source) = match piped {
        Some(stdin) => {
            let token = stdin_token(stdin)?;
            ensure_token_file_readable(&paths.token_file)?;
            (token, Token::Piped)
        }
        None => (
            FleetToken::read(&paths.token_file).map_err(key_failure)?,
            Token::InFile,
        ),
    };
    let (key, client) = connect(paths, cloud_url, token)?;
    Ok((source, key, client))
}

fn connect(
    paths: &Paths,
    cloud_url: &str,
    token: FleetToken,
) -> Result<(CheckinKey, CheckinClient), Error> {
    let key = CheckinKey::load_or_create(&paths.key()).map_err(key_failure)?;
    let client = CheckinClient::new(cloud_url, token).map_err(|error| {
        Error::caused(
            RpcErrorCode::Internal,
            "Could not build the Cloud client.",
            error,
        )
    })?;
    Ok((key, client))
}

fn stdin_token(mut stdin: impl Read + IsTerminal) -> Result<FleetToken, Error> {
    if stdin.is_terminal() {
        return Err(Error::usage(
            "--token-stdin reads a piped Fleet Token, not one typed at a terminal",
        ));
    }
    let mut text = String::new();
    stdin.read_to_string(&mut text).map_err(|error| {
        Error::caused(
            RpcErrorCode::InvalidArgument,
            "Could not read the Fleet Token from stdin.",
            error,
        )
    })?;
    FleetToken::parse(&text)
        .ok_or_else(|| Error::usage("stdin doesn't hold a Fleet Token (pflt_…)"))
}

fn ensure_token_file_readable(token_file: &Path) -> Result<(), Error> {
    replaces_token(token_file, |_| false).map(drop)
}

fn replaces_token(token_file: &Path, sends: impl Fn(&FleetToken) -> bool) -> Result<bool, Error> {
    match FleetToken::read(token_file) {
        Ok(held) => Ok(!sends(&held)),
        Err(checkin::KeyError::Token { source, .. })
            if source.kind() == io::ErrorKind::NotFound =>
        {
            Ok(false)
        }
        Err(checkin::KeyError::NotAToken { .. }) => Ok(true),
        Err(checkin::KeyError::Token { path, source }) => Err(Error::caused(
            RpcErrorCode::InvalidArgument,
            format!(
                "Could not read {}, so this install can't tell whether it replaces a Fleet Token. Fix or remove that file, then run the install again.",
                path.display()
            ),
            source,
        )),
        Err(error) => Err(key_failure(error)),
    }
}

#[derive(Clone, Copy)]
struct Install<'a> {
    cloud_url: &'a str,
    url_source: Option<&'static str>,
    token: Token,
}

#[derive(Clone, Copy)]
enum Token {
    /// Read from --token-file, where it stays.
    InFile,
    /// Piped on stdin, and written to --token-file once Cloud takes it.
    Piped,
}

#[derive(Debug, PartialEq, Eq)]
enum Answered {
    Waiting,
    Held(Hold, String),
    Unconfirmed(String),
}

async fn install(
    client: &mut CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    install_with: Install<'_>,
) -> Result<(), Error> {
    let cloud_url = install_with.cloud_url;
    let host = System::with(install_ployzd(paths, process_env::var_os("PATH").as_deref()).await?);
    let ployz = process_env::current_exe().map_err(|error| {
        Error::caused(
            RpcErrorCode::Internal,
            "Could not find this ployz binary.",
            error,
        )
    })?;
    let skip_zfs = process_env::var_os(SKIP_ZFS).is_some_and(|value| value == "1");
    let checkin_unit = unit::checkin(&ployz, &host.ployzd, &paths.token_file, cloud_url)
        .map_err(|why| Error::coded(RpcErrorCode::InvalidArgument, why))?;
    let prepare_unit = unit::prepare(&host.ployzd, skip_zfs)
        .map_err(|why| Error::coded(RpcErrorCode::InvalidArgument, why))?;
    crate::ui::stream("Checking in");
    let next = checkin::install_pass(&host, client, key, paths, Utc::now()).await;
    let answered = keep_answered_token(next, client, &paths.token_file, install_with)?;
    write_unit(unit::CHECKIN, &checkin_unit)?;
    write_unit(unit::PREPARE, &prepare_unit)?;
    systemd(&["daemon-reload"]).await?;
    checkin::restart_preparation(&host, paths, Utc::now()).await;
    systemd(&["enable", unit::CHECKIN]).await?;
    // A loop already running checks in with the old token until it starts over.
    systemd(&["restart", unit::CHECKIN]).await?;
    if answered == Answered::Waiting {
        crate::ui::stream("Waiting for approval");
    }
    Ok(())
}

fn keep_answered_token(
    next: Next,
    client: &CheckinClient,
    token_file: &Path,
    install_with: Install<'_>,
) -> Result<Answered, Error> {
    let answered = install_outcome(next, install_with)?;
    let unconfirmed = match &answered {
        Answered::Waiting => None,
        Answered::Unconfirmed(why) | Answered::Held(_, why) => Some(why),
    };
    match install_with.token {
        Token::InFile => keep_token_private(token_file)?,
        Token::Piped => {
            let _lock = lock_token_file(token_file)?;
            if let Some(why) = unconfirmed
                && replaces_token(token_file, |held| client.sends(held))?
            {
                let kept = format!(
                    "this install keeps {} and changes nothing else",
                    token_file.display()
                );
                let message = if let Answered::Held(Hold::Full, _) = answered {
                    format!(
                        "{why} Cloud didn't take the new Fleet Token, so {kept}. Run the install again with another token, or revoke this one to clear its waiting servers."
                    )
                } else {
                    format!(
                        "{why} Until Cloud takes the new Fleet Token, {kept}. Run the install again."
                    )
                };
                return Err(Error::coded(RpcErrorCode::Unavailable, message));
            }
            client.write_token(token_file).map_err(|error| {
                Error::caused(
                    RpcErrorCode::Internal,
                    format!(
                        "Could not write the Fleet Token to {}.",
                        token_file.display()
                    ),
                    error,
                )
            })?;
        }
    }
    if let Some(why) = unconfirmed {
        crate::ui::warn(format!("{why} The check-in service keeps trying."));
    }
    Ok(answered)
}

struct TokenFileLock {
    _directory: fs::File,
}

fn lock_token_file(token_file: &Path) -> Result<TokenFileLock, Error> {
    let directory = token_file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let locked = fs::create_dir_all(directory)
        .and_then(|()| fs::File::open(directory))
        .and_then(|handle| {
            rustix::fs::flock(&handle, rustix::fs::FlockOperation::LockExclusive)?;
            Ok(TokenFileLock { _directory: handle })
        });
    locked.map_err(|error| {
        Error::caused(
            RpcErrorCode::Internal,
            format!("Could not lock {}.", directory.display()),
            error,
        )
    })
}

fn install_outcome(next: Next, install_with: Install<'_>) -> Result<Answered, Error> {
    let cloud_url = install_with.cloud_url;
    match next {
        Next::Wait { log, .. } => {
            if let Some(line) = log {
                crate::ui::note(line);
            }
            Ok(Answered::Waiting)
        }
        Next::Held { hold, why, .. } => Ok(Answered::Held(hold, why)),
        Next::Retry { why, .. } => Ok(Answered::Unconfirmed(why)),
        Next::RefusedOnce { .. } => Err(Error::coded(
            RpcErrorCode::Unauthenticated,
            match install_with.url_source {
                Some(setting) => format!(
                    "Cloud at {cloud_url} refused this Fleet Token. Check the token and {setting}, then run the install again."
                ),
                None => format!(
                    "Cloud at {cloud_url} refused this Fleet Token. Check the token, then run the install again."
                ),
            },
        )),
        Next::Stop(reason, message) => {
            let again = if reason == StopReason::Rejected {
                ""
            } else {
                " Run the install again with a new token."
            };
            Err(Error::coded(
                RpcErrorCode::Unauthenticated,
                format!("{}. {message}{again}", reason.title()),
            ))
        }
        Next::ClockOff(why) => Err(Error::coded(
            RpcErrorCode::Unauthenticated,
            format!("{why}, then run the install again."),
        )),
        Next::NotCloud(why) => Err(Error::coded(
            RpcErrorCode::InvalidArgument,
            match install_with.url_source {
                Some(setting) => format!(
                    "{why}, or {setting} doesn't name Ployz Cloud. Check {setting}, then run the install again."
                ),
                None => format!("{why}. Run the install again once this host reaches Ployz Cloud."),
            },
        )),
    }
}

/// The ployzd the loop runs with, and why it should be replaced when it is older than this CLI.
#[derive(Debug, PartialEq, Eq)]
struct LoopPloyzd {
    path: PathBuf,
    stale: Option<String>,
}

/// The token, then the ployzd, the loop runs with. A missing token ends the loop before it looks
/// for a ployzd it would not use. `stopper` only disables the loop.
async fn loop_start<H: checkin::Host>(
    stopper: &H,
    matches: &ArgMatches,
    paths: &Paths,
    search: Option<&OsStr>,
) -> Result<Option<(LoopPloyzd, FleetToken)>, Error> {
    let Some(token) = checkin::loop_token(stopper, paths)
        .await
        .map_err(key_failure)?
    else {
        return Ok(None);
    };
    match loop_ployzd(matches, paths, search).await {
        Ok(ployzd) => Ok(Some((ployzd, token))),
        Err(why) => {
            checkin::no_ployzd(paths, &why, Utc::now());
            Err(Error::coded(RpcErrorCode::NotFound, format!("{why}.")))
        }
    }
}

/// The ployzd `--install` pinned, else one on PATH, else the copy install fetched, preferring one
/// as new as this CLI, then an older one that answers the probe, in the same order. An older one
/// still runs the loop, which reports it, since a loop that exits leaves systemd to give up on
/// it. Only an install fetches.
async fn loop_ployzd(
    matches: &ArgMatches,
    paths: &Paths,
    search: Option<&OsStr>,
) -> Result<LoopPloyzd, String> {
    let found: Vec<PathBuf> = matches
        .get_one::<PathBuf>("ployzd")
        .cloned()
        .into_iter()
        .chain(candidates(paths, search))
        .filter(|path| path.is_file())
        .collect();
    for path in &found {
        if crate::provisioning::daemon_is_current(path).await {
            return Ok(LoopPloyzd {
                path: path.clone(),
                stale: None,
            });
        }
    }
    let mut probing = None;
    for path in &found {
        if System::probes(path).await {
            probing = Some(path.clone());
            break;
        }
    }
    let Some(path) = probing.or_else(|| found.into_iter().next()) else {
        return Err(format!(
            "No ployzd on PATH or at {}. Run sudo ployz server checkin --install",
            paths.checkin_dir.join("ployzd").display()
        ));
    };
    let stale = format!(
        "{} is older than this ployz {}. Run sudo ployz server checkin --install",
        path.display(),
        env!("CARGO_PKG_VERSION")
    );
    Ok(LoopPloyzd {
        path,
        stale: Some(stale),
    })
}

/// A ployzd as new as this CLI, which knows the flags this one uses, fetched when none is here.
async fn install_ployzd(paths: &Paths, search: Option<&OsStr>) -> Result<PathBuf, Error> {
    for found in candidates(paths, search) {
        if found.is_file() && crate::provisioning::daemon_is_current(&found).await {
            return Ok(found);
        }
    }
    let copy = paths.checkin_dir.join("ployzd");
    crate::provisioning::fetch_daemon(&copy)
        .await
        .map_err(|error| {
            Error::caused(RpcErrorCode::Unavailable, "Could not fetch ployzd.", error)
        })?;
    Ok(copy)
}

fn candidates(paths: &Paths, search: Option<&OsStr>) -> impl Iterator<Item = PathBuf> {
    on_path(search, "ployzd")
        .into_iter()
        .chain([paths.checkin_dir.join("ployzd")])
}

fn keep_token_private(path: &Path) -> Result<(), Error> {
    let failed = |error| {
        Error::caused(
            RpcErrorCode::Internal,
            format!("Could not make {} readable by root only.", path.display()),
            error,
        )
    };
    let mode = fs::metadata(path).map_err(failed)?.permissions().mode();
    if mode & 0o077 == 0 {
        return Ok(());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(TOKEN_MODE)).map_err(failed)?;
    crate::ui::note(format!("Made {} readable by root only.", path.display()));
    Ok(())
}

fn on_path(search: Option<&OsStr>, name: &str) -> Option<PathBuf> {
    process_env::split_paths(search?)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

fn write_unit(name: &str, text: &str) -> Result<(), Error> {
    let path = Path::new(UNIT_DIR).join(name);
    fs::write(&path, text).map_err(|error| {
        Error::caused(
            RpcErrorCode::Internal,
            format!("Could not write {}.", path.display()),
            error,
        )
    })
}

async fn systemd(args: &[&str]) -> Result<(), Error> {
    checkin::systemctl(args)
        .await
        .map_err(|why| Error::coded(RpcErrorCode::Internal, why))
}

fn key_failure(error: checkin::KeyError) -> Error {
    let message = error.to_string();
    match error {
        checkin::KeyError::Token { source, .. } => {
            Error::caused(RpcErrorCode::InvalidArgument, message, source)
        }
        checkin::KeyError::NotAToken { .. } => Error::usage(message),
        checkin::KeyError::Read { source, .. } | checkin::KeyError::Write { source, .. } => {
            Error::caused(RpcErrorCode::Internal, message, source)
        }
        checkin::KeyError::Malformed { .. } => Error::coded(RpcErrorCode::Internal, message),
    }
}

#[cfg(test)]
#[path = "checkin_tests.rs"]
mod tests;
