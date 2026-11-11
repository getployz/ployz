//! `ployz server upgrade-policy set`: the Organization's upgrade policy, which Cloud keeps.
//!
//! On a Cluster whose Servers enforce device approval, choosing a Release Channel also
//! signs it with this CLI's Device key. Cloud keeps the signed channel and sends it with
//! every upgrade, and each Server upgrades through Cloud only along it. Cloud cannot sign
//! one, so it cannot move the Cluster onto another channel.

use clap::{ArgMatches, Command, value_parser};
use ployz_core::authority::{Signed, SignerKey, UpgradeChannel};
use ployz_core::{ReleaseChannel, RpcErrorCode};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::super::deploy::approve::{self, Probe};
use super::super::{Error, github_rule, leaf_matches, runtime};
use crate::cli::value;
use crate::cloud_account::{self, Credential, Via};

pub(in crate::handlers) fn group(command: Command) -> Command {
    command.about("Show or change how your servers upgrade")
}

pub(in crate::handlers) fn set_command(command: Command) -> Command {
    command
        .about("Change the Release Channel or automatic upgrades")
        .long_about(
            "Change the Release Channel or automatic upgrades. When your servers enforce \
             device approval, --channel also approves the channel with this device's key, \
             and your servers upgrade through Ployz Cloud only to its releases.",
        )
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

/// The policy Cloud keeps, as its `upgrade-channel` route reads it.
#[derive(Deserialize)]
struct Policy {
    #[serde(default)]
    approved: Option<Approved>,
}

/// The channel a person last approved, as Cloud keeps it.
#[derive(Clone, Copy, Deserialize)]
struct Approved {
    channel: ReleaseChannel,
    generation: u64,
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
    let credential = super::signed_in(&runtime, matches)?.ok_or_else(|| {
        Error::coded(
            RpcErrorCode::Unauthenticated,
            "Sign in with ployz login, then try again.",
        )
    })?;
    let approval = match channel {
        Some(channel) => approve(root, &runtime, &credential, channel)?,
        None => None,
    };
    let mut shown = Map::new();
    if let Some(approval) = &approval {
        let kept: Value = runtime.block_on(cloud_account::call(
            &credential,
            Method::POST,
            "upgrade-channel",
            Some(&json!({ "approval": approval })),
        ))?;
        shown.extend(kept.as_object().cloned().unwrap_or_default());
    }
    let mut input = Map::new();
    if approval.is_none()
        && let Some(channel) = channel
    {
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
    let approved = serde_json::from_value::<Policy>(shown.clone())
        .ok()
        .and_then(|policy| policy.approved)
        .filter(|_| approval.is_some());
    let sentence = match (approved, channel, automatic) {
        (
            Some(Approved {
                channel,
                generation,
            }),
            ..,
        ) => format!("Approved {channel} releases (approval {generation})."),
        (None, Some(channel), _) => format!("Servers upgrade to {channel} releases."),
        (None, None, Some(true)) => "Servers upgrade automatically.".to_owned(),
        (None, None, _) => "Servers upgrade only when you start one.".to_owned(),
    };
    crate::ui::done(&shown, message.unwrap_or(sentence))
}

/// `channel` signed with this CLI's Device key, when the Servers enforce device approval:
/// one generation past the approval Cloud keeps. `None` when they don't.
fn approve(
    root: &ArgMatches,
    runtime: &tokio::runtime::Runtime,
    credential: &Credential,
    channel: ReleaseChannel,
) -> Result<Option<Signed<UpgradeChannel>>, Error> {
    let enforcing = match approve::probe(root) {
        Probe::Enforcing(enforcing) => enforcing,
        Probe::Off => return Ok(None),
        Probe::NoClusterKey | Probe::Unknown => {
            crate::ui::warn(
                "Couldn't reach your servers, so the channel isn't approved. If they enforce \
                 device approval, run this again once they're reachable.",
            );
            return Ok(None);
        }
    };
    let signer = github_rule::signer(root).map_err(|_| {
        Error::coded(
            RpcErrorCode::Unauthenticated,
            "Your servers enforce device approval, and this CLI has no device key. Run \
             ployz device request, then approve it from an approved device.",
        )
    })?;
    let kept: Policy = runtime.block_on(cloud_account::call(
        credential,
        Method::GET,
        "upgrade-channel",
        None,
    ))?;
    let generation = kept
        .approved
        .map_or(1, |approved| approved.generation.saturating_add(1));
    let statement = UpgradeChannel {
        cluster: enforcing.cluster,
        channel,
        generation,
        signer: SignerKey::Device(signer.public().into()),
    };
    Signed::sign(&statement, &signer)
        .map(Some)
        .map_err(Error::command)
}
