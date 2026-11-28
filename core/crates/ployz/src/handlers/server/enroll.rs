//! `ployz server add`: enroll a Server into the signed-in Organization's Cluster through Cloud.
//!
//! The first Server founds the Cluster. Signed in, the CLI asks Cloud for an enrollment token,
//! then installs over SSH (`DESTINATION`), enrolls the host it runs on, or prints one line to
//! paste on the Server (`--command`).
//! A hidden `--standalone` founds or joins a Cluster without Cloud.

use std::future::Future;
use std::io::BufRead;
use std::path::PathBuf;
use std::time::Duration;

use clap::{ArgMatches, Command};
use ipnet::Ipv4Net;
use ployz_core::{CloudEnrollToken, ClusterName, Hint, MachineId, RpcErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::{Error, config_path, leaf_matches, runtime};
use crate::cli::{Rerun, base, positional, switch, value};
use crate::cloud;
use crate::cloud::account::Credential;
use crate::cloud::enroll::EnrollStage;
use crate::cloud::login::{CredentialStore, LoginError, Organization};
use crate::cloud::origin::{CloudOrigin, DEFAULT_CLOUD};
use crate::enrollment::EnrollPlan;
use crate::enrollment::plan::Shaping;
use crate::env;
use crate::provisioning::Installation;

const INSTALLER_URL: &str = "https://ployz.sh/";
const JOIN_POLL: Duration = Duration::from_secs(2);

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
        value("key", None)
            .help("Trust this key, from the line --command printed, when this Server joins")
            .conflicts_with_all(["destination", "command", "wait", "standalone"]),
    )
    .arg(
        value("wait", None)
            .value_name("ENROLLMENT")
            .help("Wait for the Server that runs a printed command to join")
            .conflicts_with_all(["destination", "standalone"]),
    )
    .arg(
        crate::cli::cloud_url()
            .help("Cloud that issued the enrollment token [default: ployz.dev]"),
    )
    .arg(
        value("network", None)
            .default_value(ployz_core::DEFAULT_CLUSTER_NETWORK.to_string())
            .value_parser(clap::value_parser!(Ipv4Net))
            .help("Cluster network, when this Server founds the Cluster"),
    )
    .arg(
        value("cluster-name", None)
            .value_name("NAME")
            .help("Name the new Cluster, when this Server starts one"),
    )
    .arg(switch("ingress-closed", None).help(
        "Start the new Cluster with ports 80 and 443 closed, when this Server starts one; \
         open a Door after with ployz ingress use",
    ))
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
    let cloud = crate::cli::cloud_url_of(matches)?.unwrap_or(DEFAULT_CLOUD);
    if let Some(token) = enroll_token(matches)? {
        return enroll(root, token, cloud);
    }
    let store = CredentialStore::beside(&config_path(matches)?);
    let runtime = runtime()?;
    // PLOYZ_TOKEN or this device's sign-in, as every Cloud command: one Organization.
    let credential = match runtime.block_on(cloud::account::from_env(&store)) {
        Err(LoginError::SignedOut)
            if !matches.get_flag("command") && matches.get_one::<String>("wait").is_none() =>
        {
            drop(runtime);
            let remote = matches.get_one::<String>("destination").is_some();
            return enroll(root, prompted_token(remote)?, cloud);
        }
        credential => credential?,
    };
    let acting = runtime.block_on(async {
        let organization = cloud::account::acting_in(&credential).await?;
        Ok::<_, LoginError>(Acting {
            credential,
            organization,
        })
    })?;
    if let Some(enrollment) = matches.get_one::<String>("wait") {
        return runtime.block_on(wait_joined(&acting, enrollment, another(root)));
    }
    // Before Cloud mints a token, so a bad flag costs none.
    let shape = Shaping::from_matches(matches).pasted_args()?;
    let minted: Minted = runtime.block_on(acting.post("servers/enroll", json!({})))?;
    if matches.get_flag("command") {
        let key = ployz_core::first_key::LineKey::Device {
            key: super::super::device::own_key(&config_path(matches)?)?,
        };
        return runtime.block_on(paste(&acting, &minted, &shape, &key, another(root)));
    }
    drop(runtime);
    enroll(
        root,
        CloudEnrollToken::parse(minted.token)?,
        acting.credential.cloud(),
    )
}

/// Enroll with `token`: over SSH to `DESTINATION`, or on the host this runs on.
fn enroll(root: &ArgMatches, token: CloudEnrollToken, cloud_url: &str) -> Result<(), Error> {
    let plan = EnrollPlan::from_matches(root);
    if plan.destination().is_some() {
        // A remote host is provisioned up front, in the enrollment's first step.
        return enroll_as_add(&plan, &token, cloud_url, &|_| async { Ok(()) });
    }
    if let Some(line) = leaf_matches(root).get_one::<String>("key") {
        crate::first_key_line::arm(
            ployz_core::MachineLayout::production().data_dir(),
            &crate::first_key_line::parse(line)?,
        )?;
    }
    enroll_as_add(&plan, &token, cloud_url, &|installation| async move {
        match installation {
            Installation::Host(storage) => {
                crate::provisioning::provision_local(env!("CARGO_PKG_VERSION"), &storage).await?;
            }
            Installation::Daemon => crate::provisioning::synchronize_local_daemon().await?,
        }
        Ok(())
    })
}

/// Run token enrollment, installing this CLI's daemon version with `install`.
///
/// # Errors
///
/// Returns the same CLI failure as `ployz server add` given an enrollment token.
#[doc(hidden)]
pub fn enroll_with_installer<Install, InstallFuture>(
    root: &ArgMatches,
    install: &Install,
) -> Result<(), Error>
where
    Install: Fn(Installation) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    let matches = leaf_matches(root);
    let token = enroll_token(matches)?.ok_or_else(|| {
        Error::usage(
            "Pass the enrollment token with --token-stdin, --token-file or PLOYZ_ENROLL_TOKEN",
        )
    })?;
    let cloud_url = crate::cli::cloud_url_of(matches)?.unwrap_or(DEFAULT_CLOUD);
    enroll_as_add(&EnrollPlan::from_matches(root), &token, cloud_url, install)
}

/// Enroll as `server add` does, with the Cloud enrollment's follow-up failure as its own.
fn enroll_as_add<Install, InstallFuture>(
    plan: &EnrollPlan,
    token: &CloudEnrollToken,
    cloud_url: &str,
    install: &Install,
) -> Result<(), Error>
where
    Install: Fn(Installation) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    let enrollment = crate::enrollment::cloud::Enrollment {
        plan,
        token,
        cloud_url,
        install,
        progress: &|stage| {
            if stage == EnrollStage::Enrolling && !crate::ui::json() {
                crate::ui::note("Asking Cloud to enroll this Server…");
            }
        },
    };
    runtime()?
        .block_on(enrollment.run())
        .and_then(|enrolled| enrolled.follow_up.map_or(Ok(()), Err))
}

/// The enrollment token from its one source: `--token-stdin`, `--token-file` or
/// `PLOYZ_ENROLL_TOKEN`. None of them is argv, so the token stays out of `ps`.
fn enroll_token(matches: &ArgMatches) -> Result<Option<CloudEnrollToken>, Error> {
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
    ("pflt_", "a Fleet Token, which Ployz Cloud no longer takes"),
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
        cloud::account::call(&self.credential, reqwest::Method::POST, path, Some(&body)).await
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

async fn paste(
    acting: &Acting,
    minted: &Minted,
    shape: &[String],
    key: &ployz_core::first_key::LineKey,
    another: Rerun,
) -> Result<(), Error> {
    let line = pasted_command(acting.credential.cloud(), shape, key);
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
    wait_joined(acting, &minted.id, another).await
}

/// One line that installs exactly this CLI's release, so the Server speaks the same
/// enrollment, keeping every flag that shapes the Server. It carries no token: `server add`
/// asks for it, since the pipe feeds only `sh` and leaves the terminal on its stdin, or reads
/// an exported `PLOYZ_ENROLL_TOKEN`, which plain `sudo` would drop. It carries `key`, this
/// computer's Device, for the Server to trust when it joins.
fn pasted_command(cloud: &str, shape: &[String], key: &ployz_core::first_key::LineKey) -> String {
    let cloud_flag = if CloudOrigin::is_default(cloud) {
        String::new()
    } else {
        format!(" --cloud-url '{cloud}'")
    };
    let shape_flags: String = shape
        .iter()
        .map(|arg| format!(" {}", shell_words::quote(arg)))
        .collect();
    format!(
        "curl -fsSL {INSTALLER_URL} | sh -s -- {} && sudo --preserve-env={} ployz server add{cloud_flag} --key {key}{shape_flags}",
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

/// `server add --command` as this command line gave it: another enrollment to wait on.
fn another(root: &ArgMatches) -> Rerun {
    Rerun::again(root).without("wait").switch("command")
}

async fn wait_joined(acting: &Acting, enrollment: &str, another: Rerun) -> Result<(), Error> {
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
                .hint(another));
            }
            read => match read? {
                Enrollment::Pending => tokio::time::sleep(JOIN_POLL).await,
                Enrollment::Expired => {
                    return Err(Error::coded(
                        RpcErrorCode::Conflict,
                        "the enrollment expired before a Server joined",
                    )
                    .hint(another));
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
        if founding_cluster_name(leaf_matches(root))?.is_some() {
            crate::enrollment::plan::joined_unnamed();
        }
        if leaf_matches(root).get_flag("ingress-closed") {
            crate::enrollment::plan::joined_open();
        }
        super::add(root)
    } else {
        super::init(root)
    }
}

pub(in crate::handlers) fn founding_cluster_name(
    matches: &ArgMatches,
) -> Result<Option<ClusterName>, Error> {
    crate::enrollment::plan::parse_cluster_name(
        matches
            .get_one::<String>("cluster-name")
            .map(String::as_str),
    )
}

/// Every input the enrollment engine reads from `root`, in the order it reads them, as text:
/// the first refusal, or the parsed values and flags.
#[cfg(test)]
fn engine_view(root: &ArgMatches) -> String {
    let plan = EnrollPlan::from_matches(root);
    let shaping = plan.shaping();
    let parsed = (|| -> Result<_, Error> {
        let policy = shaping.policy()?;
        let name = shaping.name()?;
        let cluster = shaping.cluster_name()?;
        let request = shaping.token_request()?;
        Ok((policy, name, cluster, shaping.storage(), request))
    })()
    .map_err(|error| format!("{error} {:?}", error.hints()));
    let reach = leaf_matches(root);
    let text = |id: &str| reach.get_one::<String>(id).cloned();
    format!(
        "{parsed:?} {:?} {:?}",
        (
            plan.destination().map(str::to_owned),
            text("ssh-key"),
            plan.no_install(),
            shaping.reset(),
            shaping.yes(),
            shaping.ingress_closed(),
            plan.shows_recovery_code(),
        ),
        (
            shaping.wg_mtu(),
            Some(shaping.network()),
            Some(shaping.version()),
            text("ployz-config"),
            crate::cli::connection(reach).map(str::to_owned),
            crate::cli::ssh_timeout(reach).as_secs(),
            text("context"),
        )
    )
}

#[cfg(test)]
mod tests {
    use clap::parser::ValueSource;

    use super::*;

    const SHAPING_SWITCHES: [&str; 3] = ["reset", "yes", "ingress-closed"];
    const SHAPING_VALUES: [&str; 13] = [
        "name",
        "storage",
        "public-ip",
        "wg-endpoint",
        "wg-mtu",
        "label-add",
        "region",
        "accepts-builds",
        "accepts-services",
        "accepts-ingress",
        "network",
        "cluster-name",
        "version",
    ];

    fn shaping_args(matches: &ArgMatches) -> Result<Vec<String>, Error> {
        Shaping::from_matches(matches).pasted_args()
    }

    fn add_matches(argv: &[&str]) -> ArgMatches {
        crate::cli::command().try_get_matches_from(argv).unwrap()
    }

    /// `argv` with the config path the command line would default to given outright, so a
    /// `PLOYZ_CONFIG` in the environment the tests run under cannot reach the views.
    fn with_default_config<'a>(argv: &[&'a str]) -> Vec<&'a str> {
        let mut argv = argv.to_vec();
        if !argv.contains(&"--ployz-config") {
            argv.extend(["--ployz-config", "~/.config/ployz/config.yaml"]);
        }
        argv
    }

    #[tokio::test]
    async fn an_enrollment_voided_by_forgetting_the_servers_ends_the_wait() {
        let cloud = crate::cloud::account::testing::fake_cloud(|route, _| match route {
            "POST /api/cli/servers/enrollment" => (
                404,
                json!({ "_tag": "PublicError", "code": "NOT_FOUND", "message": "The requested resource was not found." }),
            ),
            _ => (500, json!({})),
        });
        let acting = Acting {
            credential: Credential::Token {
                cloud,
                token: crate::cloud::login::Secret::new("ployz_secret".to_owned()),
                from: crate::cloud::account::TokenFrom::Env,
            },
            organization: Organization {
                id: "o1".to_owned(),
                slug: "acme".to_owned(),
            },
        };
        let waiting = crate::cli::command()
            .try_get_matches_from(["ployz", "server", "add", "--wait", "e1"])
            .unwrap();
        let error = wait_joined(&acting, "e1", another(&waiting))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "the enrollment ended before a Server joined: forgetting this Organization's Servers voids its token"
        );
        assert_eq!(
            error.hints(),
            [crate::cli::retry_hint("ployz server add --command")]
        );
    }

    /// The Device key a `--command` line carries in these tests.
    fn device_line() -> ployz_core::first_key::LineKey {
        ployz_core::first_key::LineKey::Device {
            key: ployz_core::DeviceKey::from_bytes([4; 32]),
        }
    }

    #[test]
    fn pasted_command_pins_this_release_and_names_a_non_default_cloud() {
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(
            pasted_command("https://ployz.dev", &[], &device_line()),
            format!(
                "curl -fsSL https://ployz.sh/ | sh -s -- {version} && sudo --preserve-env=PLOYZ_ENROLL_TOKEN ployz server add --key {}",
                device_line()
            )
        );
        assert!(
            pasted_command("http://localhost:3000", &[], &device_line()).ends_with(&format!(
                "ployz server add --cloud-url 'http://localhost:3000' --key {}",
                device_line()
            ))
        );
    }

    #[test]
    fn only_the_default_cloud_written_exactly_is_left_unnamed_in_the_pasted_line() {
        for cloud in [
            "ployz.dev",
            "https://ployz.dev/",
            "HTTPS://ployz.dev",
            "http://ployz.dev",
            "https://ployz.dev:443",
            "https://www.ployz.dev",
            "",
        ] {
            assert!(
                pasted_command(cloud, &[], &device_line())
                    .contains(&format!(" --cloud-url '{cloud}'")),
                "{cloud}"
            );
        }
        assert!(!pasted_command("https://ployz.dev", &[], &device_line()).contains("--cloud-url"));
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
            "--region",
            "EU",
            "--accepts-builds",
            "false",
            "--accepts-services=false",
            "--accepts-ingress",
            "false",
            "--network",
            "10.9.0.0/16",
            "--cluster-name",
            "acme",
            "--version",
            "0.3.0",
            "--reset",
            "--yes",
            "--ingress-closed",
        ];
        let original =
            add_matches(&[["ployz", "server", "add", "--command"].as_slice(), &given].concat());
        let original = leaf_matches(&original);
        let line = pasted_command(
            "https://ployz.dev",
            &shaping_args(original).unwrap(),
            &device_line(),
        );
        let argv = shell_words::split(line.split_once(" && ").unwrap().1).unwrap();
        let replayed = crate::cli::command()
            .try_get_matches_from(argv.iter().skip_while(|arg| *arg != "ployz"))
            .unwrap();
        let replayed = leaf_matches(&replayed);

        assert!(!replayed.get_flag("command"));
        assert_eq!(
            replayed.get_one::<String>("key"),
            Some(&device_line().to_string())
        );
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

    /// What `server add --command` pastes for `extra`, or why it refuses before minting a token.
    fn pasted(extra: &[&str]) -> Result<String, String> {
        let argv = [&["ployz", "server", "add", "--command"][..], extra].concat();
        let root = add_matches(&argv);
        shaping_args(leaf_matches(&root))
            .map(|shape| pasted_command("https://ployz.dev", &shape, &device_line()))
            .map_err(|error| format!("{error} {:?}", error.hints()))
    }

    fn shaped(extra: &[&str]) -> Result<Vec<String>, String> {
        let argv = [&["ployz", "server", "add", "--command"][..], extra].concat();
        let root = add_matches(&argv);
        shaping_args(leaf_matches(&root)).map_err(|error| format!("{error} {:?}", error.hints()))
    }

    #[test]
    fn the_pasted_line_repeats_each_given_flag_as_the_command_line_wrote_it() {
        let cases: [(&[&str], &[&str]); 18] = [
            (&[], &[]),
            (&["--name", "web-1"], &["--name", "web-1"]),
            (&["--name=web-1"], &["--name", "web-1"]),
            (
                &["--accepts-builds=false", "--accepts-services", "false"],
                &["--accepts-builds", "false", "--accepts-services", "false"],
            ),
            (
                &["--label-add", "a=b", "--label-add", "c=d"],
                &["--label-add", "a=b", "--label-add", "c=d"],
            ),
            (
                &["--label-add", "a=b,c=d"],
                &["--label-add", "a=b", "--label-add", "c=d"],
            ),
            (
                &[
                    "--wg-endpoint",
                    "192.0.2.1,192.0.2.2:7",
                    "--wg-endpoint",
                    "[2001:db8::1]:6000",
                ],
                &[
                    "--wg-endpoint",
                    "192.0.2.1",
                    "--wg-endpoint",
                    "192.0.2.2:7",
                    "--wg-endpoint",
                    "[2001:db8::1]:6000",
                ],
            ),
            (&["--public-ip", "auto"], &["--public-ip", "auto"]),
            (&["--public-ip", "none"], &["--public-ip", "none"]),
            (
                &["--network", "10.210.0.0/16"],
                &["--network", "10.210.0.0/16"],
            ),
            (&["--wg-mtu", "01380"], &["--wg-mtu", "01380"]),
            (&["--storage", "none"], &["--storage", "none"]),
            (
                &["--storage", "zfs:/dev/disk/by-id/virtio data"],
                &["--storage", "zfs:/dev/disk/by-id/virtio data"],
            ),
            (&["--version", "0.3.0"], &["--version", "0.3.0"]),
            (&["--cluster-name", "acme"], &["--cluster-name", "acme"]),
            (
                &["--ingress-closed", "--yes", "--reset"],
                &["--reset", "--yes", "--ingress-closed"],
            ),
            (&["-y"], &["--yes"]),
            (
                &[
                    "--no-install",
                    "--ssh-key",
                    "/k",
                    "--show-recovery-code",
                    "--cloud-url",
                    "x",
                ],
                &[],
            ),
        ];
        for (given, expected) in cases {
            assert_eq!(shaped(given).unwrap(), expected, "{given:?}");
        }
    }

    #[test]
    fn the_pasted_line_orders_values_before_switches_and_quotes_each_word() {
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(
            pasted(&[
                "--ingress-closed",
                "--yes",
                "--version",
                "0.3.0",
                "--label-add",
                "k=a b",
                "--storage",
                "zfs:/dev/x y",
                "--name",
                "web-1",
            ])
            .unwrap(),
            format!(
                "curl -fsSL https://ployz.sh/ | sh -s -- {version} && sudo --preserve-env=PLOYZ_ENROLL_TOKEN ployz server add --key {} --name web-1 --storage 'zfs:/dev/x y' --label-add 'k=a b' --version 0.3.0 --yes --ingress-closed",
                device_line()
            )
        );
    }

    #[test]
    fn a_flag_that_enrollment_would_refuse_is_refused_before_a_token_is_minted() {
        let cases: [(&[&str], &str); 11] = [
            (
                &["--label-add", "x", "--public-ip", "999", "--name", "BAD"],
                "invalid label: expected KEY=VALUE []",
            ),
            (
                &[
                    "--public-ip",
                    "999",
                    "--name",
                    "BAD",
                    "--cluster-name",
                    "BAD",
                ],
                "invalid public IP: expected an IPv4 or IPv6 address []",
            ),
            (
                &["--cluster-name", "BAD", "--name", "BAD"],
                "invalid Machine Name: a 1-63 character lowercase DNS label [Closest(\"bad\")]",
            ),
            (
                &["--label-add", "x"],
                "invalid label: expected KEY=VALUE []",
            ),
            (
                &["--label-add", "a=1", "--label-add", "a=2"],
                "duplicate label key \"a\" []",
            ),
            (
                &["--public-ip", "999"],
                "invalid public IP: expected an IPv4 or IPv6 address []",
            ),
            (
                &["--public-ip", "127.0.0.1"],
                "127.0.0.1 is a loopback address, which no visitor can reach. Give the address the internet reaches this Server at, or none. []",
            ),
            (
                &["--wg-endpoint", "nope"],
                "invalid WireGuard endpoint: expected IP or IP:PORT []",
            ),
            (
                &["--name", "MY SERVER"],
                "invalid Machine Name: a 1-63 character lowercase DNS label [Closest(\"my-server\")]",
            ),
            (
                &["--name", ""],
                "invalid Machine Name: a 1-63 character lowercase DNS label []",
            ),
            (
                &["--cluster-name", "Not A Name"],
                "Use up to 63 lowercase letters, digits and hyphens. Start and end with a letter or digit. []",
            ),
        ];
        for (given, expected) in cases {
            assert_eq!(shaped(given).unwrap_err(), expected, "{given:?}");
        }
    }

    #[test]
    fn the_engine_reads_what_the_command_line_gave_and_the_defaults_of_the_rest() {
        let cases: [&[&str]; 6] = [
            &["--token-stdin"],
            &[
                "--token-stdin",
                "root@host",
                "-i",
                "/k",
                "--no-install",
                "--ssh-timeout",
                "7",
            ],
            &[
                "--token-stdin",
                "--reset",
                "--yes",
                "--ingress-closed",
                "--show-recovery-code",
            ],
            &[
                "--token-stdin",
                "--wg-mtu",
                "1380",
                "--network",
                "10.9.0.0/16",
                "--version",
                "0.3.0",
            ],
            &[
                "--token-stdin",
                "--name",
                "web-1",
                "--storage",
                "zfs:/dev/x",
                "--accepts-ingress=false",
                "--label-add",
                "a=b",
                "--cluster-name",
                "acme",
                "--public-ip",
                "192.0.2.9",
                "--wg-endpoint",
                "192.0.2.1",
            ],
            &[
                "--ployz-config",
                "/c.yaml",
                "--connect",
                "tcp://127.0.0.1:9",
                "--context",
                "x",
                "--token-stdin",
            ],
        ];
        let views: String = cases
            .iter()
            .map(|given| {
                let argv = [&["ployz", "server", "add"][..], given].concat();
                format!(
                    "{given:?} => {}\n",
                    engine_view(&add_matches(&with_default_config(&argv)))
                )
            })
            .collect();
        assert_eq!(views, include_str!("enroll_views.golden"));
    }

    #[test]
    fn an_environment_sourced_version_is_pasted_and_a_default_is_not() {
        const CHILD: &str = "PLOYZ_PIN_ENV_CHILD";
        if std::env::var_os(CHILD).is_none() {
            for (version, pasted_flag) in [(Some("9.9.9"), true), (None, false)] {
                let mut child = std::process::Command::new(std::env::current_exe().unwrap());
                child
                    .args([
                        "--exact",
                        "handlers::server::enroll::tests::an_environment_sourced_version_is_pasted_and_a_default_is_not",
                        "--nocapture",
                    ])
                    .env(CHILD, if pasted_flag { "given" } else { "default" })
                    .env_remove(env::DAEMON_VERSION);
                if let Some(version) = version {
                    child.env(env::DAEMON_VERSION, version);
                }
                let output = child.output().unwrap();
                assert!(
                    output.status.success(),
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
            }
            return;
        }
        let shape = shaped(&[]).unwrap();
        if std::env::var(CHILD).unwrap() == "given" {
            assert_eq!(shape, ["--version", "9.9.9"]);
        } else {
            assert!(shape.is_empty(), "{shape:?}");
        }
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
