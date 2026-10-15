//! `ployz server add`: enroll a Server into the signed-in Organization's Cluster through Cloud.
//!
//! The first Server founds the Cluster. Signed in, the CLI asks Cloud for an enrollment token,
//! then installs over SSH (`DESTINATION`), enrolls the host it runs on, or prints one line to
//! paste on the Server (`--command`). The pasted line pipes the token to `server add --token-stdin`.
//! A hidden `--standalone` founds or joins a Cluster without Cloud, for tests.

use std::io::BufRead;
use std::path::PathBuf;
use std::time::Duration;

use clap::{ArgMatches, Command};
use ipnet::Ipv4Net;
use ployz_core::{CloudEnrollToken, MachineId, RpcErrorCode, StorageChoice};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::{Error, config_path, leaf_matches, runtime};
use crate::cli::{base, env, positional, switch, value};
use crate::cloud_account::{self, Credential};
use crate::cloud_login::{CredentialStore, DEFAULT_CLOUD, LoginError, Organization};
use crate::ui::Hint;

const INSTALLER_URL: &str = "https://ployz.sh/";
const JOIN_POLL: Duration = Duration::from_secs(2);

pub(crate) fn command(command: Command) -> Command {
    super::provisioning_flags(
        base(command, "Add a Server to your Organization's Cluster")
            .long_about("Add a Server to your Organization's Cluster through Cloud; the first Server founds it. Needs `ployz login`. With DESTINATION, installs Ployz over SSH; without one, adds the Server this runs on. --command prints one line to paste on the Server and waits until it joins; with --json it returns the line at once, and --wait ENROLLMENT waits."),
    )
    .arg(positional("destination", false))
    .arg(
        switch("token-stdin", None)
            .help("Read the enrollment token from Cloud as one line on stdin; the pasted command pipes it")
            .conflicts_with_all(["token-file", "command", "wait", "standalone"]),
    )
    .arg(
        value("token-file", None)
            .value_parser(clap::value_parser!(PathBuf))
            .help(format!("Read the enrollment token from a file; {} also passes it", env::ENROLL_TOKEN))
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
            .help("Cloud that issued the enrollment token [default: ployz.dev]"),
    )
    .arg(
        value("network", None)
            .default_value("10.210.0.0/16")
            .value_parser(clap::value_parser!(Ipv4Net))
            .help("Cluster network, when this Server founds the Cluster"),
    )
    .arg(
        value("ingress-image", None)
            .value_parser(crate::ingress::pinned_image)
            .help("Caddy image to deploy when founding a Cluster"),
    )
    .arg(switch("reset", None).help("Reset an initialized Server before enrollment"))
    .arg(
        switch("standalone", None)
            .hide(true)
            .help("Found or join a Cluster without Cloud"),
    )
}

pub(crate) fn add(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    if matches.get_flag("standalone") {
        return standalone(root);
    }
    if let Some(token) = enroll_token(matches)? {
        let cloud = matches
            .get_one::<String>("cloud-url")
            .map_or(DEFAULT_CLOUD, String::as_str);
        return super::super::cloud::enroll(root, token, cloud);
    }
    let store = CredentialStore::beside(&config_path(matches)?);
    let runtime = runtime()?;
    // PLOYZ_TOKEN or this device's sign-in, as every Cloud command: one Organization.
    let acting = runtime.block_on(async {
        let credential = cloud_account::from_env(&store).await?;
        let organization = cloud_account::acting_in(&credential).await?;
        Ok::<_, LoginError>(Acting {
            credential,
            organization,
        })
    })?;
    if let Some(enrollment) = matches.get_one::<String>("wait") {
        return runtime.block_on(wait_joined(&acting, enrollment));
    }
    let minted: Minted = runtime.block_on(acting.post("servers/enroll", json!({})))?;
    if matches.get_flag("command") {
        let storage = matches.get_one::<StorageChoice>("storage").copied();
        return runtime.block_on(paste(&acting, &minted, storage));
    }
    drop(runtime);
    super::super::cloud::enroll(
        root,
        CloudEnrollToken::parse(minted.token)?,
        acting.credential.cloud(),
    )
}

/// The enrollment token from its one source: `--token-stdin`, `--token-file` or
/// `PLOYZ_ENROLL_TOKEN`. None of them is argv, so the token stays out of `ps`.
pub(in crate::handlers) fn enroll_token(
    matches: &ArgMatches,
) -> Result<Option<CloudEnrollToken>, Error> {
    let from_env = std::env::var(env::ENROLL_TOKEN)
        .ok()
        .filter(|token| !token.is_empty());
    let from_stdin = matches.get_flag("token-stdin");
    let from_file = matches.get_one::<PathBuf>("token-file");
    if let Some(from_env) = from_env {
        if from_stdin || from_file.is_some() {
            return Err(Error::usage(format!(
                "{} is set; pass the enrollment token once, without --token-stdin or --token-file",
                env::ENROLL_TOKEN
            )));
        }
        if matches.get_flag("command")
            || matches.get_flag("standalone")
            || matches.get_one::<String>("wait").is_some()
        {
            return Err(Error::usage(format!(
                "{} enrolls with a token; unset it to use --command, --wait or --standalone",
                env::ENROLL_TOKEN
            )));
        }
        return Ok(Some(CloudEnrollToken::parse(from_env.trim())?));
    }
    let raw = if from_stdin {
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|error| {
                Error::caused(
                    RpcErrorCode::InvalidArgument,
                    "Could not read the enrollment token from stdin",
                    error,
                )
            })?;
        line
    } else if let Some(path) = from_file {
        std::fs::read_to_string(path).map_err(|error| {
            Error::caused(
                RpcErrorCode::InvalidArgument,
                format!(
                    "Could not read the enrollment token from {}",
                    path.display()
                ),
                error,
            )
        })?
    } else {
        return Ok(None);
    };
    Ok(Some(CloudEnrollToken::parse(raw.trim())?))
}

/// What a Cloud enrollment acts with, and the Organization it enrolls into.
struct Acting {
    credential: Credential,
    organization: Organization,
}

impl Acting {
    /// POST `/api/cli/PATH` with `body` plus this Organization's slug.
    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        mut body: serde_json::Value,
    ) -> Result<T, LoginError> {
        if let Some(object) = body.as_object_mut() {
            object.insert("organizationSlug".into(), json!(self.organization.slug));
        }
        cloud_account::call(&self.credential, reqwest::Method::POST, path, Some(&body)).await
    }
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

async fn paste(
    acting: &Acting,
    minted: &Minted,
    storage: Option<StorageChoice>,
) -> Result<(), Error> {
    let line = pasted_command(&minted.token, acting.credential.cloud(), storage);
    if crate::ui::json() {
        return crate::ui::emit(&PendingReport {
            status: "pending",
            enrollment: &minted.id,
            command: line,
            expires_at: &minted.expires_at,
            next: format!("ployz server add --wait {}", minted.id),
        });
    }
    crate::ui::note(format_args!(
        "Run this on the Server (expires {}):\n\n  {line}\n",
        minted.expires_at
    ));
    crate::ui::note("Waiting for the Server to join...");
    wait_joined(acting, &minted.id).await
}

/// One line that installs exactly this CLI's release, so the Server speaks the same
/// enrollment, keeping an explicit `--storage` choice. The leading space keeps it out of
/// shell history, and `printf` is a builtin, so the token never reaches any argv.
fn pasted_command(token: &str, cloud: &str, storage: Option<StorageChoice>) -> String {
    let cloud_flag = if cloud == crate::cloud_enroll::cloud_origin(DEFAULT_CLOUD) {
        String::new()
    } else {
        format!(" --cloud-url '{cloud}'")
    };
    let storage_flag = storage.map_or(String::new(), |storage| format!(" --storage {storage}"));
    format!(
        " curl -fsSL {INSTALLER_URL} | sh -s -- {} && printf '%s\\n' '{token}' | sudo ployz server add --token-stdin{cloud_flag}{storage_flag}",
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
        machine_id: MachineId,
    },
}

async fn wait_joined(acting: &Acting, enrollment: &str) -> Result<(), Error> {
    loop {
        match acting
            .post("servers/enrollment", json!({ "id": enrollment }))
            .await?
        {
            Enrollment::Pending => tokio::time::sleep(JOIN_POLL).await,
            Enrollment::Expired => {
                return Err(Error::coded(
                    RpcErrorCode::Conflict,
                    "the enrollment expired before a Server joined",
                )
                .hint(Hint::Retry("ployz server add --command".into())));
            }
            Enrollment::Joined { machine_id } => {
                crate::ui::stream("The Server joined.");
                return crate::ui::emit(
                    &json!({ "server": { "id": machine_id }, "status": "joined" }),
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
        .unwrap_or(StorageChoice::Zfs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_command_pins_this_release_and_names_a_non_default_cloud() {
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(
            pasted_command("pmet_x", "https://ployz.dev", None),
            format!(
                " curl -fsSL https://ployz.sh/ | sh -s -- {version} && printf '%s\\n' 'pmet_x' | sudo ployz server add --token-stdin"
            )
        );
        assert!(
            pasted_command("pmet_x", "http://localhost:3000", None)
                .ends_with("--token-stdin --cloud-url 'http://localhost:3000'")
        );
        assert!(
            pasted_command("pmet_x", "https://ployz.dev", Some(StorageChoice::None))
                .ends_with("--token-stdin --storage none")
        );
    }
}
