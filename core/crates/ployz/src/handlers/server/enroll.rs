//! `ployz server add`: enroll a Server into the signed-in Organization's Cluster through Cloud.
//!
//! The first Server founds the Cluster. Signed in, the CLI asks Cloud for an enrollment token,
//! then installs over SSH (`DESTINATION`), enrolls the host it runs on, or prints one line to
//! paste on the Server (`--command`). The pasted line runs `server add --token` on the Server.
//! A hidden `--standalone` founds or joins a Cluster without Cloud, for tests.

use std::time::Duration;

use clap::{ArgMatches, Command};
use ipnet::Ipv4Net;
use ployz_core::{CloudEnrollToken, RpcErrorCode, StorageChoice};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::{Error, config_path, leaf_matches, runtime};
use crate::cli::{base, env, positional, switch, value};
use crate::cloud_login::{self, CredentialStore, SignedIn};
use crate::output::say;

const DEFAULT_CLOUD: &str = "ployz.dev";
const INSTALLER_URL: &str = "https://ployz.sh/";
const JOIN_POLL: Duration = Duration::from_secs(2);

pub(super) fn command() -> Command {
    super::provisioning_flags(
        base("add", "Add a Server to your Organization's Cluster")
            .long_about("Add a Server to your Organization's Cluster through Cloud; the first Server founds it. Needs `ployz login`. With DESTINATION, installs Ployz over SSH; without one, adds the Server this runs on. --command prints one line to paste on the Server and waits until it joins; with --json it returns the line at once, and --wait ENROLLMENT waits."),
    )
    .arg(positional("destination", false))
    .arg(
        value("token", None)
            .help("Enrollment token from Cloud; the pasted command passes it")
            .conflicts_with_all(["command", "wait", "standalone"]),
    )
    .arg(
        switch("command", None)
            .help("Print one line to paste on the Server, then wait for it to join")
            .conflicts_with_all(["destination", "wait", "standalone"]),
    )
    .arg(
        value("wait", None)
            .value_name("ENROLLMENT")
            .help("Wait for the Server that runs a printed command to join")
            .conflicts_with_all(["destination", "standalone"]),
    )
    .arg(
        value("cloud-url", None)
            .env(env::CLOUD_URL)
            .help("Cloud that issued --token [default: ployz.dev]"),
    )
    .arg(
        value("network", None)
            .default_value("10.210.0.0/16")
            .value_parser(clap::value_parser!(Ipv4Net))
            .help("Cluster network, when this Server founds the Cluster"),
    )
    .arg(value("ingress-image", None).help("Caddy image to deploy when founding a Cluster"))
    .arg(switch("reset", None).help("Reset an initialized Server before enrollment"))
    .arg(
        switch("standalone", None)
            .hide(true)
            .help("Found or join a Cluster without Cloud"),
    )
}

pub(super) fn add(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    if matches.get_flag("standalone") {
        return standalone(root);
    }
    if let Some(token) = matches.get_one::<String>("token") {
        let cloud = matches
            .get_one::<String>("cloud-url")
            .map_or(DEFAULT_CLOUD, String::as_str);
        return super::super::cloud::enroll(root, CloudEnrollToken::parse(token.clone())?, cloud);
    }
    let store = CredentialStore::beside(&config_path(matches)?);
    let runtime = runtime()?;
    let signed_in = runtime.block_on(cloud_login::signed_in(&store))?;
    if let Some(enrollment) = matches.get_one::<String>("wait") {
        return runtime.block_on(wait_joined(&signed_in, enrollment));
    }
    let minted: Minted = runtime.block_on(signed_in.post(
        "/api/cli/servers/enroll",
        &json!({ "organizationSlug": signed_in.organization.slug }),
    ))?;
    if matches.get_flag("command") {
        return runtime.block_on(paste(&signed_in, &minted));
    }
    drop(runtime);
    super::super::cloud::enroll(
        root,
        CloudEnrollToken::parse(minted.token)?,
        &signed_in.cloud,
    )
}

/// A fresh enrollment token Cloud issued to this device's Organization.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Minted {
    id: String,
    token: String,
    expires_at: String,
}

#[derive(Serialize)]
struct PendingReport<'a> {
    status: &'static str,
    enrollment: &'a str,
    command: String,
    expires_at: &'a str,
    next: String,
}

async fn paste(signed_in: &SignedIn, minted: &Minted) -> Result<(), Error> {
    let line = pasted_command(&minted.token, &signed_in.cloud);
    if crate::output::json() {
        return crate::output::emit(&PendingReport {
            status: "pending",
            enrollment: &minted.id,
            command: line,
            expires_at: &minted.expires_at,
            next: format!("ployz server add --wait {}", minted.id),
        });
    }
    say!(
        "Run this on the Server (expires {}):\n\n  {line}\n",
        minted.expires_at
    );
    say!("Waiting for the Server to join...");
    wait_joined(signed_in, &minted.id).await
}

/// One line that installs exactly this CLI's release, so the Server speaks the same enrollment.
fn pasted_command(token: &str, cloud: &str) -> String {
    let cloud_flag = if cloud == crate::cloud_enroll::cloud_origin(DEFAULT_CLOUD) {
        String::new()
    } else {
        format!(" --cloud-url '{cloud}'")
    };
    format!(
        "curl -fsSL {INSTALLER_URL} | sh -s -- {} && sudo ployz server add --token '{token}'{cloud_flag}",
        env!("CARGO_PKG_VERSION")
    )
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Enrollment {
    Pending,
    Expired,
    Joined {
        #[serde(rename = "machineId")]
        machine_id: String,
    },
}

async fn wait_joined(signed_in: &SignedIn, enrollment: &str) -> Result<(), Error> {
    let body = json!({ "organizationSlug": signed_in.organization.slug, "id": enrollment });
    loop {
        match signed_in.post("/api/cli/servers/enrollment", &body).await? {
            Enrollment::Pending => tokio::time::sleep(JOIN_POLL).await,
            Enrollment::Expired => {
                return Err(Error::detailed(
                    RpcErrorCode::Conflict,
                    "the enrollment expired before a Server joined",
                    json!({ "next": "ployz server add --command" }),
                ));
            }
            Enrollment::Joined { machine_id } => {
                say!("Server {machine_id} joined");
                return crate::output::emit(
                    &json!({ "status": "joined", "machine_id": machine_id }),
                );
            }
        }
    }
}

/// Without Cloud: found a new context's Cluster, or join the selected context's.
fn standalone(root: &ArgMatches) -> Result<(), Error> {
    let options = super::ConnectionOptions::from_matches(root)?;
    let config = options.load_or_empty_config()?;
    let joins = config
        .context_name(options.context())
        .is_some_and(|name| config.contexts.contains_key(name));
    if joins {
        super::add(root)
    } else {
        super::init(root)
    }
}

/// Storage the Cloud path asks for; it never prompts.
pub(in crate::handlers) fn requested_storage(matches: &ArgMatches) -> StorageChoice {
    matches
        .get_one::<StorageChoice>("storage")
        .copied()
        .unwrap_or(StorageChoice::None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_command_pins_this_release_and_names_a_non_default_cloud() {
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(
            pasted_command("pmet_x", "https://ployz.dev"),
            format!(
                "curl -fsSL https://ployz.sh/ | sh -s -- {version} && sudo ployz server add --token 'pmet_x'"
            )
        );
        assert!(
            pasted_command("pmet_x", "http://localhost:3000")
                .ends_with("--token 'pmet_x' --cloud-url 'http://localhost:3000'")
        );
    }
}
