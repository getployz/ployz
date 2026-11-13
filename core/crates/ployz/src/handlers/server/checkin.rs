//! `ployz server checkin`: a waiting host checks in with Cloud until it is approved.
//!
//! `--install` writes the loop and preparation units, checks in once, and leaves the loop
//! running under systemd. Without it, this is the loop the unit runs.

use std::{
    env as process_env,
    io::{IsTerminal, Read},
    path::PathBuf,
};

use clap::{ArgMatches, Command, parser::ValueSource};
use ployz_core::{Hint, MachineLayout, RpcErrorCode};

use super::super::{Error, leaf_matches, runtime};
use crate::checkin::{
    self, CheckinClient, CheckinEnd, CheckinKey, FleetToken, Paths, System,
    install::{self, Install, Token},
    key_failure,
};
use crate::cli::{base, switch, value};
use crate::cloud_origin::DEFAULT_CLOUD;
use crate::env;

pub(crate) fn command(command: Command) -> Command {
    base(command, "Check in with Cloud until this host is approved")
        .long_about("Check in with Cloud until this host is approved. --install writes the ployz-checkin and ployz-prepare units, checks in once, and leaves the loop running; it ends when Cloud enrolls or rejects the host, or the Fleet Token is revoked or expires. Without --install, this runs the loop in the foreground.")
        .arg(switch("install", None).help("Install the check-in units, check in once, and leave the loop running"))
        .arg(
            value("token-file", None)
                .value_parser(clap::value_parser!(PathBuf))
                .default_value(MachineLayout::production().fleet_token().into_os_string())
                .help("Read the Fleet Token from this file. The running check-in loop deletes it once Cloud enrolls or rejects this host, or the token is revoked or expires; an --install that ends with an error keeps it"),
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
        .unwrap_or_else(|| MachineLayout::production().fleet_token());
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
    if !crate::checkin::process_is_root() {
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
        return runtime.block_on(install::install(&mut client, &key, &paths, install_with));
    }
    // Held before startup writes a record: a second loop overwrites nothing, and a failed start
    // reads as stopped.
    let _running = match checkin::hold_loop(&paths) {
        Ok(Some(running)) => Some(running),
        Ok(None) => {
            return Err(
                Error::coded(RpcErrorCode::Conflict, "A check-in loop already runs here.")
                    .hint(Hint::Inspect("sudo systemctl status ployz-checkin".into())),
            );
        }
        // Its records then say it ran unlocked, so `status` claims nothing about it.
        Err(error) => {
            crate::ui::warn(format!("Could not hold the check-in loop's lock: {error}"));
            None
        }
    };
    let Some((ployzd, token)) = runtime.block_on(install::loop_start(
        &System::with(PathBuf::new()),
        matches.get_one::<PathBuf>("ployzd").cloned(),
        &paths,
        process_env::var_os("PATH").as_deref(),
    ))?
    else {
        return Ok(());
    };
    let host = System {
        ployzd: ployzd.path,
        stale: ployzd.stale,
        cloud: cloud_url.clone(),
    };
    let (key, mut client) = connect(&paths, &cloud_url, token)?;
    match runtime.block_on(checkin::run(&host, &mut client, &key, &paths)) {
        CheckinEnd::Stopped(_) | CheckinEnd::Enrolled => Ok(()),
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
            checkin::ensure_token_readable(&paths.token_file)?;
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

#[cfg(test)]
#[path = "checkin_tests.rs"]
mod tests;
