//! `ployz server upgrade-policy set`: the Organization's upgrade policy, which Cloud keeps.

use clap::{ArgMatches, Command, value_parser};
use ployz_core::{ReleaseChannel, RpcErrorCode};
use serde_json::{Map, Value, json};

use super::super::{Error, leaf_matches, runtime};
use crate::cli::value;
use crate::cloud::http::Via;

pub(in crate::handlers) fn group(command: Command) -> Command {
    command.about("Show or change how your servers upgrade")
}

pub(in crate::handlers) fn set_command(command: Command) -> Command {
    command
        .about("Change the Release Channel or automatic upgrades")
        .arg(
            value("channel", None)
                .value_parser(["stable", "beta"])
                .help("The Release Channel every upgrade follows"),
        )
        .arg(
            value("automatic", None)
                .value_parser(value_parser!(bool))
                .help("Whether Servers behind the channel upgrade on their own"),
        )
}

pub(in crate::handlers) fn set(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let channel = matches
        .get_one::<String>("channel")
        .map(|channel| match channel.as_str() {
            "beta" => ReleaseChannel::Beta,
            _ => ReleaseChannel::Stable,
        });
    let automatic = matches.get_one::<bool>("automatic").copied();
    if channel.is_none() && automatic.is_none() {
        return Err(Error::usage("Give --channel, --automatic, or both."));
    }
    let runtime = runtime()?;
    let credential = super::super::route::signed_in(&runtime, matches)?.ok_or_else(|| {
        Error::coded(
            RpcErrorCode::Unauthenticated,
            "Sign in with ployz login, then try again.",
        )
    })?;
    let mut shown = Map::new();
    let mut input = Map::new();
    if let Some(channel) = channel {
        input.insert("channel".into(), json!(channel));
    }
    if let Some(automatic) = automatic {
        input.insert("automatic".into(), json!(automatic));
    }
    let mut message = None;
    if !input.is_empty() {
        let ran = runtime
            .block_on(crate::published::run_on(
                &credential,
                Via::Cli,
                "server_upgrade_policy_set",
                Value::Object(input),
                None,
                &uuid::Uuid::new_v4().to_string(),
            ))
            .0?;
        shown.extend(ran.output.as_object().cloned().unwrap_or_default());
        message = ran.message;
    }
    let shown = Value::Object(shown);
    let sentence = match (channel, automatic) {
        (Some(channel), _) => format!("Servers upgrade to {channel} releases."),
        (None, Some(true)) => "Servers upgrade automatically.".to_owned(),
        (None, _) => "Servers upgrade only when you start one.".to_owned(),
    };
    crate::ui::done(&shown, message.unwrap_or(sentence))
}
