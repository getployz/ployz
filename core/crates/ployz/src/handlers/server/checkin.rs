//! `ployz server checkin`: a waiting host checks in with Cloud until it is approved.
//!
//! `--install` writes the loop and preparation units, checks in once, and leaves the loop
//! running under systemd. Without it, this is the loop the unit runs.

use std::{
    env as process_env, fs,
    path::{Path, PathBuf},
};

use chrono::Utc;
use clap::{ArgMatches, Command};
use ployz_core::RpcErrorCode;

use super::super::{Error, leaf_matches, runtime};
use crate::checkin::{
    self, CheckinClient, CheckinEnd, CheckinKey, FleetToken, Next, Paths, System, unit,
};
use crate::cli::{base, env, switch, value};
use crate::cloud_login::DEFAULT_CLOUD;

const UNIT_DIR: &str = "/etc/systemd/system";
const SKIP_ZFS: &str = "PLOYZ_SKIP_ZFS";

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
            value("cloud-url", None)
                .env(env::CLOUD_URL)
                .help("Cloud to check in with [default: ployz.dev]"),
        )
}

pub(crate) fn checkin(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let token_file = matches
        .get_one::<PathBuf>("token-file")
        .cloned()
        .unwrap_or_else(|| PathBuf::from(Paths::TOKEN_FILE));
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
    let token = FleetToken::read(&paths.token_file).map_err(key_failure)?;
    let key = CheckinKey::load_or_create(&paths.key()).map_err(key_failure)?;
    let client = CheckinClient::new(&cloud_url, token).map_err(|error| {
        Error::caused(
            RpcErrorCode::Internal,
            "Could not build the Cloud client.",
            error,
        )
    })?;
    let runtime = runtime()?;
    if matches.get_flag("install") {
        runtime.block_on(install(&client, &key, &paths, &cloud_url))
    } else {
        let host = System {
            ployzd: runtime.block_on(ployzd(&paths, false))?,
        };
        let CheckinEnd::Stopped(_) = runtime.block_on(checkin::run(&host, &client, &key, &paths));
        Ok(())
    }
}

async fn install(
    client: &CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    cloud_url: &str,
) -> Result<(), Error> {
    let host = System {
        ployzd: ployzd(paths, true).await?,
    };
    let ployz = process_env::current_exe().map_err(|error| {
        Error::caused(
            RpcErrorCode::Internal,
            "Could not find this ployz binary.",
            error,
        )
    })?;
    let skip_zfs = process_env::var_os(SKIP_ZFS).is_some_and(|value| value == "1");
    write_unit(
        unit::CHECKIN,
        &unit::checkin(&ployz, &paths.token_file, cloud_url),
    )?;
    write_unit(unit::PREPARE, &unit::prepare(&host.ployzd, skip_zfs))?;
    systemd(&["daemon-reload"]).await?;
    crate::ui::stream("Checking in");
    match checkin::pass(&host, client, key, paths, Utc::now()).await {
        Next::Wait { log, .. } => {
            if let Some(line) = log {
                crate::ui::note(line);
            }
        }
        Next::Retry { why, .. } => {
            crate::ui::warn(format!("{why} The loop keeps trying."));
        }
        Next::Stop(reason, message) => {
            checkin::stop(&host, paths).await;
            return Err(Error::coded(
                RpcErrorCode::Unauthenticated,
                format!("{}. {message}", reason.title()),
            ));
        }
    }
    systemd(&["enable", "--now", unit::CHECKIN]).await?;
    crate::ui::stream("Waiting for approval");
    Ok(())
}

/// The `ployzd` that probes and prepares: the one on `PATH`, else this release's copy
/// in the check-in directory, fetched when `fetch` allows.
async fn ployzd(paths: &Paths, fetch: bool) -> Result<PathBuf, Error> {
    if let Some(found) = on_path("ployzd") {
        return Ok(found);
    }
    let copy = paths.checkin_dir.join("ployzd");
    if copy.is_file() {
        return Ok(copy);
    }
    if !fetch {
        return Err(Error::coded(
            RpcErrorCode::NotFound,
            format!(
                "No ployzd on PATH or at {}. Run ployz server checkin --install.",
                copy.display()
            ),
        ));
    }
    crate::provisioning::fetch_daemon(&copy)
        .await
        .map_err(|error| {
            Error::caused(RpcErrorCode::Unavailable, "Could not fetch ployzd.", error)
        })?;
    Ok(copy)
}

fn on_path(name: &str) -> Option<PathBuf> {
    let path = process_env::var_os("PATH")?;
    process_env::split_paths(&path)
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
