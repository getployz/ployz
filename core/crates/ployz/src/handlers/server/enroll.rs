//! `ployz server add`: enroll a Server into the signed-in Organization's Cluster through Cloud.
//!
//! The first Server founds the Cluster. Signed in, the CLI asks Cloud for an enrollment token,
//! then installs over SSH (`DESTINATION`), enrolls the host it runs on, or prints one line to
//! paste on the Server (`--command`).
//! A hidden `--standalone` founds or joins a Cluster without Cloud.

use std::io::BufRead;
use std::path::PathBuf;
use std::time::Duration;

use clap::parser::ValueSource;
use clap::{ArgMatches, Command};
use ipnet::Ipv4Net;
use ployz_core::{CloudEnrollToken, MachineId, RpcErrorCode, StoragePreparation};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::{Error, config_path, leaf_matches, runtime};
use crate::cli::{base, env, positional, switch, value};
use crate::cloud_account::{self, Credential};
use crate::cloud_login::{CredentialStore, DEFAULT_CLOUD, LoginError, Organization};
use crate::ui::Hint;

const INSTALLER_URL: &str = "https://ployz.sh/";
const JOIN_POLL: Duration = Duration::from_secs(2);

#[path = "enroll_waiting.rs"]
mod waiting;
pub(crate) use waiting::{command as waiting_command, enroll as enroll_waiting};

pub(crate) fn command(command: Command) -> Command {
    super::provisioning_flags(
        base(command, "Add a Server to your Organization")
            .long_about("Add a Server to your Organization through Cloud.\n\nWith DESTINATION, installs Ployz over SSH; without one, adds the Server this runs on.\n\nSigned in, it asks Cloud for an enrollment token. Signed out, it asks for one at a hidden prompt, or reads --token-stdin, --token-file or PLOYZ_ENROLL_TOKEN.\n\n--command prints one line to paste on the Server and the token beside it, then waits until it joins. With --json it returns both at once; --wait ENROLLMENT waits."),
    )
    .arg(
        positional("destination", false)
            .help("SSH destination such as root@host to install on; omit to add this Server"),
    )
    .arg(
        switch("token-stdin", None)
            .help("Read the enrollment token from Cloud as one line on stdin")
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
        switch("show-recovery-code", None)
            .help("Print the recovery code under --json when founding a Cluster"),
    )
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
    // Before any token is asked for or minted, so a mistyped destination costs none.
    if let Some(destination) = matches.get_one::<String>("destination") {
        destination.parse::<crate::context::Connection>()?;
    }
    let cloud = matches
        .get_one::<String>("cloud-url")
        .map_or(DEFAULT_CLOUD, String::as_str);
    if let Some(token) = enroll_token(matches)? {
        return super::super::cloud::enroll(root, token, cloud);
    }
    let store = CredentialStore::beside(&config_path(matches)?);
    let runtime = runtime()?;
    // PLOYZ_TOKEN or this device's sign-in, as every Cloud command: one Organization.
    let credential = match runtime.block_on(cloud_account::from_env(&store)) {
        Err(LoginError::SignedOut)
            if !matches.get_flag("command") && matches.get_one::<String>("wait").is_none() =>
        {
            drop(runtime);
            let remote = matches.get_one::<String>("destination").is_some();
            return super::super::cloud::enroll(root, prompted_token(remote)?, cloud);
        }
        credential => credential?,
    };
    let acting = runtime.block_on(async {
        let organization = cloud_account::acting_in(&credential).await?;
        Ok::<_, LoginError>(Acting {
            credential,
            organization,
        })
    })?;
    if let Some(enrollment) = matches.get_one::<String>("wait") {
        return runtime.block_on(wait_joined(&acting, enrollment));
    }
    // Before Cloud mints a token, so a bad flag costs none.
    let shape = shaping_args(matches)?;
    let minted: Minted = runtime.block_on(acting.post("servers/enroll", json!({})))?;
    if matches.get_flag("command") {
        return runtime.block_on(paste(&acting, &minted, &shape));
    }
    drop(runtime);
    super::super::cloud::enroll(
        root,
        CloudEnrollToken::parse(minted.token)?,
        acting.credential.cloud(),
    )
}

/// `server add` switches and values that shape the Server, which a pasted line repeats.
const SHAPING_SWITCHES: [&str; 2] = ["reset", "yes"];
const SHAPING_VALUES: [&str; 12] = [
    "name",
    "storage",
    "public-ip",
    "wg-endpoint",
    "wg-mtu",
    "label-add",
    "accepts-builds",
    "accepts-services",
    "accepts-ingress",
    "network",
    "ingress-image",
    "version",
];

/// The flags given here that shape the Server, as `server add` arguments, once each
/// parses as enrollment will read it.
fn shaping_args(matches: &ArgMatches) -> Result<Vec<String>, Error> {
    super::enrollment_policy(matches)?;
    super::helpers::checked_token_request(matches)?;
    super::helpers::requested_name(matches)?;
    let given = |id: &str| {
        matches!(
            matches.value_source(id),
            Some(ValueSource::CommandLine | ValueSource::EnvVariable)
        )
    };
    let switches = SHAPING_SWITCHES
        .into_iter()
        .filter(|id| given(id) && matches.get_flag(id))
        .map(|id| format!("--{id}"));
    let values = SHAPING_VALUES
        .into_iter()
        .filter(|id| given(id))
        .flat_map(|id| {
            matches
                .get_raw(id)
                .into_iter()
                .flatten()
                .flat_map(move |raw| [format!("--{id}"), raw.to_string_lossy().into_owned()])
        });
    Ok(values.chain(switches).collect())
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
        return parse_enroll_token(&from_env, env::ENROLL_TOKEN).map(Some);
    }
    let (raw, from) = if from_stdin {
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
        (line, "--token-stdin")
    } else if let Some(path) = from_file {
        let raw = std::fs::read_to_string(path).map_err(|error| {
            Error::caused(
                RpcErrorCode::InvalidArgument,
                format!(
                    "Could not read the enrollment token from {}",
                    path.display()
                ),
                error,
            )
        })?;
        (raw, "--token-file")
    } else {
        return Ok(None);
    };
    parse_enroll_token(&raw, from).map(Some)
}

/// Tokens Cloud mints for other jobs, by prefix, and where each belongs.
const OTHER_TOKENS: [(&str, &str); 3] = [
    (
        "pflt_",
        "a Fleet Token. Set PLOYZ_FLEET_TOKEN instead: the installer reads it there",
    ),
    (
        "ployz_",
        "an Organization Token. Set PLOYZ_TOKEN instead: Cloud commands read it there",
    ),
    ("ppair_", "a pairing secret, which no command takes"),
];

/// `raw`, read from `from`, as an enrollment token. A token of another kind is refused
/// here, before anything reaches a Server or Cloud.
fn parse_enroll_token(raw: &str, from: &str) -> Result<CloudEnrollToken, Error> {
    let raw = raw.trim();
    if let Some((_, kind)) = OTHER_TOKENS
        .into_iter()
        .find(|(prefix, _)| raw.starts_with(prefix))
    {
        return Err(Error::usage(format!(
            "{from} holds {kind}. ployz server add takes the enrollment token that ployz server add --command prints."
        )));
    }
    Ok(CloudEnrollToken::parse(raw)?)
}

fn prompted_token(remote: bool) -> Result<CloudEnrollToken, Error> {
    let token = crate::ui::secret(
        "Enrollment token",
        || no_terminal_for_token(remote),
        "Cancelled. Nothing was enrolled.",
    )?;
    parse_enroll_token(&token, "What you pasted")
}

/// Run from a script, nothing can answer the prompt. Adding a remote Server, signing in
/// here is the way on; on the Server itself, `sudo` drops an exported
/// `PLOYZ_ENROLL_TOKEN`, so name the form that survives it.
fn no_terminal_for_token(remote: bool) -> Error {
    Error::coded(
        RpcErrorCode::Unauthenticated,
        format!(
            "No terminal to ask for the enrollment token. Pass it with --token-stdin, --token-file or {}.",
            env::ENROLL_TOKEN
        ),
    )
    .hint(Hint::Next(
        if remote {
            "ployz login"
        } else {
            "sudo ployz server add --token-stdin"
        }
        .into(),
    ))
}

/// What a Cloud enrollment acts with, and the Organization it enrolls into.
struct Acting {
    credential: Credential,
    organization: Organization,
}

impl Acting {
    /// POST `/api/cli/PATH` with `body` plus this Organization's ID.
    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        mut body: serde_json::Value,
    ) -> Result<T, LoginError> {
        if let Some(object) = body.as_object_mut() {
            object.insert("organizationId".into(), json!(self.organization.id));
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
    token: &'a str,
    expires_at: &'a str,
    next: String,
}

async fn paste(acting: &Acting, minted: &Minted, shape: &[String]) -> Result<(), Error> {
    let line = pasted_command(acting.credential.cloud(), shape);
    if crate::ui::json() {
        return crate::ui::emit(&PendingReport {
            status: "pending",
            enrollment: &minted.id,
            command: line,
            token: &minted.token,
            expires_at: &minted.expires_at,
            next: format!("ployz server add --wait {}", minted.id),
        });
    }
    crate::ui::note(format_args!(
        "Run this on the Server, and paste the token when it asks (expires {}). From a script, export {} with the token first; the line keeps it through sudo.\n\n  {line}\n\n  {}\n",
        minted.expires_at,
        env::ENROLL_TOKEN,
        minted.token
    ));
    wait_joined(acting, &minted.id).await
}

/// One line that installs exactly this CLI's release, so the Server speaks the same
/// enrollment, keeping every flag that shapes the Server. It carries no token: `server add`
/// asks for it, since the pipe feeds only `sh` and leaves the terminal on its stdin, or reads
/// an exported `PLOYZ_ENROLL_TOKEN`, which plain `sudo` would drop.
fn pasted_command(cloud: &str, shape: &[String]) -> String {
    let cloud_flag = if cloud == crate::cloud_enroll::cloud_origin(DEFAULT_CLOUD) {
        String::new()
    } else {
        format!(" --cloud-url '{cloud}'")
    };
    let shape_flags: String = shape
        .iter()
        .map(|arg| format!(" {}", shell_words::quote(arg)))
        .collect();
    format!(
        "curl -fsSL {INSTALLER_URL} | sh -s -- {} && sudo --preserve-env={} ployz server add{cloud_flag}{shape_flags}",
        env!("CARGO_PKG_VERSION"),
        env::ENROLL_TOKEN
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
    crate::ui::note("Waiting for the Server to join...");
    loop {
        let read = acting
            .post("servers/enrollment", json!({ "id": enrollment }))
            .await;
        match read {
            Err(LoginError::Status { status: 404, .. }) => {
                return Err(Error::coded(
                    RpcErrorCode::NotFound,
                    "the enrollment ended before a Server joined: forgetting this Organization's Servers voids its token",
                )
                .hint(Hint::Retry("ployz server add --command".into())));
            }
            read => match read? {
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
            },
        }
    }
}

/// Without Cloud: found a new context's Cluster, or join the selected context's.
fn standalone(root: &ArgMatches) -> Result<(), Error> {
    let options = super::ConnectionOptions::from_matches(root)?;
    let config = options.load_or_empty_config()?;
    let joins = config
        .context_name(options.context())
        .and_then(|name| config.contexts.get(name))
        .is_some_and(|context| !context.connections.is_empty());
    if joins {
        super::add(root)
    } else {
        super::init(root)
    }
}

/// Storage the Cloud path asks for; it never prompts.
pub(in crate::handlers) fn requested_storage(matches: &ArgMatches) -> StoragePreparation {
    matches
        .get_one::<StoragePreparation>("storage")
        .cloned()
        .unwrap_or(StoragePreparation::Zfs { device: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add_matches(argv: &[&str]) -> ArgMatches {
        crate::cli::command().try_get_matches_from(argv).unwrap()
    }

    #[tokio::test]
    async fn an_enrollment_voided_by_forgetting_the_servers_ends_the_wait() {
        let cloud = crate::cloud_login::tests::fake_cloud(|route, _| match route {
            "POST /api/cli/servers/enrollment" => (
                404,
                json!({ "_tag": "PublicError", "code": "NOT_FOUND", "message": "The requested resource was not found." }),
            ),
            _ => (500, json!({})),
        });
        let acting = Acting {
            credential: Credential::Token {
                cloud,
                token: crate::cloud_login::Secret::new("ployz_secret".to_owned()),
            },
            organization: Organization {
                id: "o1".to_owned(),
                slug: "acme".to_owned(),
            },
        };
        let error = wait_joined(&acting, "e1").await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "the enrollment ended before a Server joined: forgetting this Organization's Servers voids its token"
        );
        assert_eq!(
            error.hints(),
            [Hint::Retry("ployz server add --command".into())]
        );
    }

    #[test]
    fn pasted_command_pins_this_release_and_names_a_non_default_cloud() {
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(
            pasted_command("https://ployz.dev", &[]),
            format!(
                "curl -fsSL https://ployz.sh/ | sh -s -- {version} && sudo --preserve-env=PLOYZ_ENROLL_TOKEN ployz server add"
            )
        );
        assert!(
            pasted_command("http://localhost:3000", &[])
                .ends_with("ployz server add --cloud-url 'http://localhost:3000'")
        );
    }

    #[test]
    fn the_pasted_line_carries_only_the_flags_given() {
        let matches = add_matches(&["ployz", "server", "add", "--command"]);
        assert_eq!(
            shaping_args(leaf_matches(&matches)).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_pasted_line_parses_to_the_same_server() {
        let given = [
            "--name",
            "web-1",
            "--storage",
            "zfs:/dev/disk/by-id/virtio data",
            "--public-ip",
            "192.0.2.9",
            "--wg-endpoint",
            "192.0.2.1,[2001:db8::1]:6000",
            "--wg-mtu",
            "1380",
            "--label-add",
            "region=eu",
            "--label-add",
            "rack=r1",
            "--accepts-builds",
            "false",
            "--accepts-services=false",
            "--accepts-ingress",
            "false",
            "--network",
            "10.9.0.0/16",
            "--ingress-image",
            "caddy:2.10",
            "--version",
            "0.3.0",
            "--reset",
            "--yes",
        ];
        let original =
            add_matches(&[["ployz", "server", "add", "--command"].as_slice(), &given].concat());
        let original = leaf_matches(&original);
        let line = pasted_command("https://ployz.dev", &shaping_args(original).unwrap());
        let argv = shell_words::split(line.split_once(" && ").unwrap().1).unwrap();
        let replayed = crate::cli::command()
            .try_get_matches_from(argv.iter().skip_while(|arg| *arg != "ployz"))
            .unwrap();
        let replayed = leaf_matches(&replayed);

        assert!(!replayed.get_flag("command"));
        let raw = |matches: &ArgMatches, id: &str| -> Vec<String> {
            matches
                .get_raw(id)
                .into_iter()
                .flatten()
                .map(|raw| raw.to_string_lossy().into_owned())
                .collect()
        };
        for id in SHAPING_VALUES {
            assert_eq!(raw(replayed, id), raw(original, id), "--{id}");
            assert_eq!(
                replayed.value_source(id),
                Some(ValueSource::CommandLine),
                "--{id}"
            );
        }
        for id in SHAPING_SWITCHES {
            assert!(replayed.get_flag(id), "--{id}");
        }
        assert_eq!(
            super::super::enrollment_policy(replayed).unwrap(),
            super::super::enrollment_policy(original).unwrap()
        );
        assert_eq!(
            super::super::helpers::token_request(replayed).unwrap(),
            super::super::helpers::token_request(original).unwrap()
        );
    }

    #[test]
    fn a_missing_terminal_names_the_scripted_token_sources() {
        let refusal = no_terminal_for_token(false);
        let message = refusal.to_string();
        assert!(!message.contains("Not signed in"), "{message}");
        assert!(message.contains("--token-stdin"), "{message}");
        assert!(message.contains(env::ENROLL_TOKEN), "{message}");
        assert!(
            refusal
                .hints()
                .iter()
                .all(|hint| *hint != Hint::Next("ployz login".into()))
        );
        assert!(
            no_terminal_for_token(true)
                .hints()
                .contains(&Hint::Next("ployz login".into()))
        );
    }
}
