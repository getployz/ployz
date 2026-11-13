//! What `ployz server checkin` does on the host around the loop: finding the ployzd it runs
//! with, writing the units and answering `--install`.

use std::{
    collections::HashSet,
    env as process_env,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use chrono::Utc;
use ployz_core::RpcErrorCode;

use super::client::key_failure;
use super::token_file::{self, FleetToken, TokenFileLock};
use super::{
    CheckinClient, CheckinKey, Hold, Host, InstallPass, NO_CLUSTER, NO_CLUSTER_WHY, Next, Paths,
    StopReason, System, install_pass, joined, loop_token, no_ployzd, record, restart_preparation,
    systemctl, unit,
};
use crate::failure::Failure;

const SKIP_ZFS: &str = "PLOYZ_SKIP_ZFS";

#[derive(Clone, Copy)]
pub(crate) struct Install<'a> {
    pub(crate) cloud_url: &'a str,
    pub(crate) url_source: Option<&'static str>,
    pub(crate) token: Token,
}

#[derive(Clone, Copy)]
pub(crate) enum Token {
    /// Read from --token-file, where it stays.
    InFile,
    /// Piped on stdin, and written to --token-file once Cloud takes it.
    Piped,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Answered {
    Waiting,
    Enrolling,
    Held(Hold, String),
    Unconfirmed(String),
}

pub(crate) async fn install(
    client: &mut CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    install_with: Install<'_>,
) -> Result<(), Failure> {
    let cloud_url = install_with.cloud_url;
    let host = System::with(install_ployzd(paths, process_env::var_os("PATH").as_deref()).await?);
    let ployz = process_env::current_exe().map_err(|error| {
        Failure::caused(
            RpcErrorCode::Internal,
            "Could not find this ployz binary.",
            error,
        )
    })?;
    let skip_zfs = process_env::var_os(SKIP_ZFS).is_some_and(|value| value == "1");
    let checkin_unit = unit::checkin(
        &ployz,
        &host.ployzd,
        &paths.token_file,
        cloud_url,
        &ployz_core::release_mirror_overrides(),
    )
    .map_err(|why| Failure::coded(RpcErrorCode::InvalidArgument, why))?;
    let prepare_unit = unit::prepare(&host.ployzd, skip_zfs)
        .map_err(|why| Failure::coded(RpcErrorCode::InvalidArgument, why))?;
    crate::ui::stream("Checking in");
    let pass = install_pass(&host, client, key, paths, Utc::now()).await;
    let mut seen = pass.seen.clone();
    let joined = joined(&host, seen.as_ref().and_then(|seen| seen.attempt.as_ref())).await;
    let answered = answer_install(pass, client, paths, install_with)?;
    unit::write(unit::CHECKIN, &checkin_unit)?;
    unit::write(unit::PREPARE, &prepare_unit)?;
    systemd(&["daemon-reload"]).await?;
    let now = Utc::now();
    if restart_preparation(&host, paths, now).await
        && let Some(seen) = &mut seen
    {
        seen.restarted_preparation(now);
    }
    systemd(&["enable", unit::CHECKIN]).await?;
    // A loop already running checks in with the old token until it starts over.
    systemd(&["restart", unit::CHECKIN]).await?;
    if answered == Answered::Waiting {
        let blockers: Vec<String> = seen.as_ref().map_or_else(Vec::new, |seen| {
            seen.blockers
                .iter()
                .map(|blocker| blocker.message.clone())
                .collect()
        });
        let no_cluster = seen.is_some_and(|seen| seen.no_cluster);
        for line in waiting_lines(joined, &blockers, no_cluster) {
            crate::ui::stream(line);
        }
    }
    crate::ui::note(
        "To approve changes from your browser, add a passkey in your Ployz Cloud account once this server joins, then run this on each server:\n  sudo ployz device approve --passkey <code>",
    );
    Ok(())
}

/// How an install Cloud answered with a wait ends: what `ployz status` says of this host.
pub(crate) fn waiting_lines(joined: bool, blockers: &[String], no_cluster: bool) -> Vec<String> {
    if joined {
        vec!["Already joined".to_owned()]
    } else if !blockers.is_empty() {
        // Two checks can give one reason, as a Docker install that failed on a small disk does.
        let mut said = HashSet::new();
        blockers
            .iter()
            .filter(|blocker| said.insert(blocker.as_str()))
            .map(|blocker| format!("Blocked. {blocker}"))
            .collect()
    } else if no_cluster {
        vec![format!("{}. {}", NO_CLUSTER, NO_CLUSTER_WHY)]
    } else {
        vec!["Waiting for approval".to_owned()]
    }
}

/// Keeps the token Cloud answered, and records the answer only for the token the install keeps. A
/// replacement Cloud didn't take leaves the held token's status, and its enrollment, untouched.
pub(crate) fn answer_install(
    pass: InstallPass,
    client: &CheckinClient,
    paths: &Paths,
    install_with: Install<'_>,
) -> Result<Answered, Failure> {
    // Held across the gate and the record, so another install can't swap the token in between.
    let _lock = match install_with.token {
        Token::Piped => Some(TokenFileLock::take(&paths.token_file)?),
        Token::InFile => None,
    };
    let answered = keep_answered_token(pass.next, client, &paths.token_file, install_with);
    if let Some(seen) = &pass.seen
        && !token_file::replaces(&paths.token_file, |held| client.sends(held)).unwrap_or(true)
    {
        record(seen, paths);
    }
    answered
}

/// A piped token is written here, so the caller holds `lock_token_file` around this.
pub(crate) fn keep_answered_token(
    next: Next,
    client: &CheckinClient,
    token_file: &Path,
    install_with: Install<'_>,
) -> Result<Answered, Failure> {
    let answered = install_outcome(next, install_with)?;
    let unconfirmed = match &answered {
        Answered::Waiting | Answered::Enrolling => None,
        Answered::Unconfirmed(why) | Answered::Held(_, why) => Some(why),
    };
    match install_with.token {
        Token::InFile => token_file::keep_private(token_file)?,
        Token::Piped => {
            if let Some(why) = unconfirmed
                && token_file::replaces(token_file, |held| client.sends(held))?
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
                return Err(Failure::coded(RpcErrorCode::Unavailable, message));
            }
            client.token().write(token_file).map_err(|error| {
                Failure::caused(
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

pub(crate) fn install_outcome(next: Next, install_with: Install<'_>) -> Result<Answered, Failure> {
    let cloud_url = install_with.cloud_url;
    match next {
        Next::Wait { .. } => Ok(Answered::Waiting),
        Next::Enrolling => {
            crate::ui::note("Cloud is ready to enroll this server. The check-in loop runs it.");
            Ok(Answered::Enrolling)
        }
        Next::Enrolled => unreachable!("an install pass leaves enrolling to the loop"),
        Next::Held { hold, why, .. } => Ok(Answered::Held(hold, why)),
        Next::Retry { why, .. } => Ok(Answered::Unconfirmed(why)),
        Next::RefusedOnce { .. } => Err(Failure::coded(
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
            Err(Failure::coded(
                RpcErrorCode::Unauthenticated,
                format!("{}. {message}{again}", reason.title()),
            ))
        }
        Next::ClockOff(why) => Err(Failure::coded(
            RpcErrorCode::Unauthenticated,
            format!("{why}, then run the install again."),
        )),
        Next::NotCloud(why) => Err(Failure::coded(
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
pub(crate) struct LoopPloyzd {
    pub(crate) path: PathBuf,
    pub(crate) stale: Option<String>,
}

/// The token, then the ployzd, the loop runs with. A missing token ends the loop before it looks
/// for a ployzd it would not use. `stopper` only disables the loop.
pub(crate) async fn loop_start<H: Host>(
    stopper: &H,
    pinned: Option<PathBuf>,
    paths: &Paths,
    search: Option<&OsStr>,
) -> Result<Option<(LoopPloyzd, FleetToken)>, Failure> {
    let Some(token) = loop_token(stopper, paths).await.map_err(key_failure)? else {
        return Ok(None);
    };
    match loop_ployzd(pinned, paths, search).await {
        Ok(ployzd) => Ok(Some((ployzd, token))),
        Err(why) => {
            no_ployzd(paths, &why, Utc::now());
            Err(Failure::coded(RpcErrorCode::NotFound, format!("{why}.")))
        }
    }
}

/// The ployzd `--install` pinned, else one on PATH, else the copy install fetched, preferring one
/// as new as this CLI, then an older one that answers the probe, in the same order. An older one
/// still runs the loop, which reports it, since a loop that exits leaves systemd to give up on
/// it. Only an install fetches.
pub(crate) async fn loop_ployzd(
    pinned: Option<PathBuf>,
    paths: &Paths,
    search: Option<&OsStr>,
) -> Result<LoopPloyzd, String> {
    let found: Vec<PathBuf> = pinned
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
async fn install_ployzd(paths: &Paths, search: Option<&OsStr>) -> Result<PathBuf, Failure> {
    for found in candidates(paths, search) {
        if found.is_file() && crate::provisioning::daemon_is_current(&found).await {
            return Ok(found);
        }
    }
    let copy = paths.checkin_dir.join("ployzd");
    crate::provisioning::fetch_daemon(&copy)
        .await
        .map_err(|error| {
            Failure::caused(RpcErrorCode::Unavailable, "Could not fetch ployzd.", error)
        })?;
    Ok(copy)
}

fn candidates(paths: &Paths, search: Option<&OsStr>) -> impl Iterator<Item = PathBuf> {
    on_path(search, "ployzd")
        .into_iter()
        .chain([paths.checkin_dir.join("ployzd")])
}

fn on_path(search: Option<&OsStr>, name: &str) -> Option<PathBuf> {
    process_env::split_paths(search?)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

async fn systemd(args: &[&str]) -> Result<(), Failure> {
    systemctl(args)
        .await
        .map_err(|why| Failure::coded(RpcErrorCode::Internal, why))
}
