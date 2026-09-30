use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
};

use clap::{Arg, ArgAction, ArgMatches, Command, ValueHint};

use crate::cli::{
    base, env, log_flags, machine_policy_flags, many, positional, switch, value, volume_acceptance,
};
use ployz_core::{
    AdvertisedEndpoint, BuildConcurrencyUpdate, Machine, MachineName, MachineTarget, MachineUpdate,
    PublicIpUpdate, RpcErrorCode, UpdateMachineRequest, op,
};

use serde_json::{Value, json};

use crate::{
    cloud_account::{self, Credential},
    cloud_login::{CredentialStore, LoginError},
    connect::{Client, SystemConnector, TARGET_RPC_TIMEOUT},
    context::{Config, ConnectionSource, ContextError, SelectedConnections},
    ingress::IngressImage,
    output::{self, say},
};

use super::{Error, leaf_matches, string_values, with_client};

mod add;
mod clean;
mod enroll;
mod helpers;
mod init;
mod inspect;
mod remove;
mod upgrade;

use add::add;
pub(super) use enroll::requested_storage;
pub(super) use helpers::{
    confirm, connect_direct, initialize, join, machine_name, readiness_timeout_message, reset,
};
use init::init;
use inspect::{inspect, list};
use remove::remove;
use upgrade::upgrade;

pub(super) fn clear_build_cache(matches: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(matches);
    if leaf.get_one::<String>("connect").is_some() || leaf.get_one::<String>("context").is_some() {
        return Err(Error::usage(
            "cache clearing runs on this execution host; run it there as the builder user without --connect or --context",
        ));
    }
    ployz_build::clear_cache(&ployz_build::HostPolicy::default())
        .map_err(|error| Error::coded(ployz_core::RpcErrorCode::Internal, error.to_string()))?;
    output::finish(&json!({ "build_cache": { "cleared": true } }), || {
        say!("Cleared this host user's Ployz build cache.");
    })
}

/// Reach the Cluster for a live operation. `--connect` or `--context` win. Otherwise, signed in
/// to Cloud or holding `PLOYZ_TOKEN`, it dials through this credential's own Server Access;
/// signed out, through the current context or the local daemon.
pub(super) async fn connect(matches: &ArgMatches, context: Option<&str>) -> Result<Client, Error> {
    if matches.get_one::<String>("connect").is_none() && context.is_none() {
        let store = CredentialStore::beside(&super::config_path(matches)?);
        match cloud_account::from_env(&store).await {
            Ok(credential) => return through_cloud(matches, &credential).await,
            Err(LoginError::SignedOut) => {
                // Signed out with no context or local daemon: the fix is signing in, not a
                // config file. The hidden test Store is never signed in, so it keeps the cause.
                return super::connect_context(matches, context)
                    .await
                    .map_err(|error| {
                        let no_config = std::error::Error::source(&error)
                            .and_then(|source| source.downcast_ref::<ContextError>())
                            .is_some_and(|source| *source == ContextError::NoConfig);
                        if no_config && !super::store::local_mode() {
                            LoginError::SignedOut.into()
                        } else {
                            error
                        }
                    });
            }
            Err(error) => return Err(error.into()),
        }
    }
    super::connect_context(matches, context).await
}

async fn through_cloud(matches: &ArgMatches, credential: &Credential) -> Result<Client, Error> {
    let access = cloud_account::server_access(credential).await?;
    if access.connections.is_empty() {
        return Err(no_reachable_server(access.unreachable));
    }
    let selected = SelectedConnections {
        source: ConnectionSource::Cloud,
        connections: access.connections,
    };
    let connector = SystemConnector::default().with_ssh_timeout(crate::cli::ssh_timeout(matches));
    Ok(crate::connect::connect_selected_with(selected, Arc::new(connector)).await?)
}

fn no_reachable_server(unreachable: Vec<ployz_core::MachineId>) -> Error {
    if unreachable.is_empty() {
        return Error::detailed(
            RpcErrorCode::NotFound,
            "this Organization has no Servers",
            json!({ "next": "ployz server add" }),
        );
    }
    Error::detailed(
        RpcErrorCode::Unavailable,
        format!(
            "no Server of this Organization is reachable now: {}",
            super::joined(&unreachable)
        ),
        json!({ "unreachable": unreachable }),
    )
}

pub(super) struct ConnectionOptions {
    config_path: PathBuf,
    context: Option<String>,
}

impl ConnectionOptions {
    pub(super) fn from_matches(matches: &ArgMatches) -> Result<Self, Error> {
        Ok(Self {
            config_path: super::config_path(matches)?,
            context: super::leaf_matches(matches)
                .get_one::<String>("context")
                .cloned(),
        })
    }

    pub(super) fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }

    pub(super) fn active_config(&self) -> Result<(Config, String), Error> {
        let config = self.load_config()?;
        let Some(name) = config
            .context_name(self.context.as_deref())
            .map(str::to_owned)
        else {
            return Err(Error::usage(format!(
                "current context is not set in Ployz config {}",
                config.path().display()
            )));
        };
        if !config.contexts.contains_key(&name) {
            return Err(Error::not_found(format!("context {name:?} not found")));
        }
        Ok((config, name))
    }

    pub(super) fn load_config(&self) -> Result<Config, Error> {
        Ok(Config::load(&self.config_path)?)
    }

    pub(super) fn load_or_empty_config(&self) -> Result<Config, Error> {
        Ok(Config::load_or_empty(&self.config_path)?)
    }
}

pub(super) fn target<'a>(matches: &'a ArgMatches, name: &str) -> Result<&'a str, Error> {
    matches
        .get_one::<String>(name)
        .map(String::as_str)
        .ok_or_else(|| Error::usage(format!("{name} is required")))
}

/// A Machine as every `server` command prints it: its WireGuard public key in base64,
/// as `wg` prints it.
pub(super) fn machine_json(machine: &Machine) -> Value {
    let mut value = serde_json::to_value(machine).expect("a Machine serializes");
    if let Value::Object(fields) = &mut value {
        fields.insert("public_key".into(), json!(machine.public_key.to_string()));
    }
    value
}

/// A Server as `server add`, `init`, `set` and `rm` print it: the `server ls` and
/// `server inspect` shape, its Machine under `machine`.
pub(super) fn server_json(machine: &Machine) -> Value {
    json!({ "machine": machine_json(machine) })
}

fn set(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let selector = target(matches, "server")?;
    let update = parse_update(matches)?;
    let image = matches.get_one::<String>("ingress-image");
    let role = format!(
        "--accepts-ingress={}",
        update.accepts_ingress.unwrap_or(true)
    );
    let mut args = vec!["server", "set", selector, role.as_str()];
    args.extend(
        image
            .iter()
            .flat_map(|image| ["--ingress-image", image.as_str()]),
    );
    let rerun = rerun(matches, &args);
    let ingress = IngressImage::given_or(image.cloned(), IngressImage::Keep);
    let accepts_ingress = update.accepts_ingress;
    let selector = MachineTarget::parse(selector)?;
    with_client(root, |client| {
        Box::pin(async move {
            let machine = client
                .invoke::<op::UpdateMachine>(
                    UpdateMachineRequest { update },
                    &selector,
                    Some(TARGET_RPC_TIMEOUT),
                )
                .await?
                .machine;
            say!("Updated Server {} ({})", machine.name, machine.id);
            // The update is committed; the Ingress Proxy following a role change is a follow-up.
            let Some(accepts) = accepts_ingress else {
                return output::emit(&json!({ "server": server_json(&machine) }));
            };
            let followed = async {
                wait_for_ingress_role(client, &machine.id, accepts).await?;
                crate::ingress::follow_roles(client, ingress).await
            }
            .await;
            output::emit_committed(
                json!({ "server": server_json(&machine), "ingress": followed.as_ref().ok().and_then(Option::as_ref) }),
                followed
                    .map(drop)
                    .map_err(|error| ingress_incomplete("Server updated", &error, rerun)),
            )
        })
    })
}

/// Wait until this entry sees the Server's new ingress role, so the deploy plans from it.
async fn wait_for_ingress_role(
    client: &mut Client,
    id: &ployz_core::MachineId,
    accepts: bool,
) -> Result<(), Error> {
    // ponytail: fixed 30 s bound; the role replicates within seconds on a healthy Cluster.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let machines = client.machines().await?;
        if machines
            .iter()
            .any(|entry| entry.machine.id == *id && entry.machine.accepts_ingress == accepts)
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::unavailable(
                "this entry Server has not yet observed the new ingress role",
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// The exact rerun of a Server command, keeping an explicit `--context`.
pub(super) fn rerun(matches: &ArgMatches, args: &[&str]) -> String {
    let context = matches.get_one::<String>("context");
    let args = std::iter::once("ployz").chain(args.iter().copied()).chain(
        context
            .map(|context| ["--context", context.as_str()])
            .into_iter()
            .flatten(),
    );
    shell_words::join(args)
}

/// A committed Server change whose Ingress Proxy follow-up failed: name it and the exact rerun.
pub(super) fn ingress_incomplete(committed: &str, error: &Error, next: String) -> Error {
    Error::detailed(
        error.report().code,
        format!("{committed}; the Ingress Proxy did not follow: {error}\nContinue with: {next}"),
        json!({ "next": next }),
    )
}

fn parse_update(matches: &ArgMatches) -> Result<MachineUpdate, Error> {
    let name = matches
        .get_one::<String>("name")
        .map(MachineName::parse)
        .transpose()?;
    let public_ip = match matches.get_one::<String>("public-ip").map(String::as_str) {
        None => PublicIpUpdate::Keep,
        Some("") | Some("none") => PublicIpUpdate::Remove,
        Some(value) => PublicIpUpdate::Set(
            value
                .parse::<IpAddr>()
                .map_err(|_| Error::usage(format!("invalid public IP {value:?}")))?,
        ),
    };
    let build_concurrency = match matches
        .get_one::<String>("build-concurrency")
        .map(String::as_str)
    {
        None => BuildConcurrencyUpdate::Keep,
        Some("auto") => BuildConcurrencyUpdate::Automatic,
        Some(value) => BuildConcurrencyUpdate::Set(value.parse()?),
    };
    let advertised_endpoints = if matches.get_many::<String>("wg-endpoint").is_some() {
        Some(parse_endpoints(&string_values(matches, "wg-endpoint"))?)
    } else {
        None
    };
    let update = MachineUpdate {
        name,
        public_ip,
        advertised_endpoints,
        build_concurrency,
        ..parse_policy(matches)?
    };
    if update.is_empty() {
        return Err(Error::usage("at least one setting flag is required"));
    }
    if update
        .advertised_endpoints
        .as_ref()
        .is_some_and(Vec::is_empty)
    {
        return Err(Error::usage("at least one WireGuard endpoint is required"));
    }
    Ok(update)
}

pub(super) fn parse_policy(matches: &ArgMatches) -> Result<MachineUpdate, Error> {
    let mut label_changes = parse_label_add(matches)?
        .into_iter()
        .map(|(key, value)| (key, Some(value)))
        .collect::<std::collections::BTreeMap<_, _>>();
    for key in string_values(matches, "label-rm") {
        let key = ployz_core::MachineLabelKey::parse(key)?;
        match label_changes.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(None);
            }
            std::collections::btree_map::Entry::Occupied(entry) if entry.get().is_some() => {
                return Err(Error::usage(format!(
                    "Server Label {} cannot be added and removed in the same patch",
                    entry.key()
                )));
            }
            std::collections::btree_map::Entry::Occupied(_) => {}
        }
    }
    Ok(MachineUpdate {
        label_changes,
        accepts_builds: matches.get_one::<bool>("accepts-builds").copied(),
        accepts_services: matches.get_one::<bool>("accepts-services").copied(),
        accepts_ingress: matches.get_one::<bool>("accepts-ingress").copied(),
        ..Default::default()
    })
}

fn parse_label_add(
    matches: &ArgMatches,
) -> Result<
    std::collections::BTreeMap<ployz_core::MachineLabelKey, ployz_core::MachineLabelValue>,
    Error,
> {
    let mut labels = std::collections::BTreeMap::new();
    for label in string_values(matches, "label-add") {
        let (key, value) = label
            .split_once('=')
            .ok_or_else(|| Error::usage(format!("invalid label {label:?}: expected KEY=VALUE")))?;
        if labels.insert(key.parse()?, value.parse()?).is_some() {
            return Err(Error::usage(format!("duplicate label key {key:?}")));
        }
    }
    Ok(labels)
}

pub(super) fn enrollment_policy(
    matches: &ArgMatches,
) -> Result<ployz_core::InitialMachinePolicy, Error> {
    Ok(ployz_core::InitialMachinePolicy {
        labels: parse_label_add(matches)?,
        accepts_builds: matches
            .get_one::<bool>("accepts-builds")
            .copied()
            .unwrap_or(true),
        accepts_services: matches
            .get_one::<bool>("accepts-services")
            .copied()
            .unwrap_or(true),
        accepts_ingress: matches
            .get_one::<bool>("accepts-ingress")
            .copied()
            .unwrap_or(true),
    })
}

pub(super) fn parse_endpoints(values: &[String]) -> Result<Vec<AdvertisedEndpoint>, Error> {
    values
        .iter()
        .map(|value| {
            value
                .parse::<SocketAddr>()
                .or_else(|_| {
                    value
                        .parse::<IpAddr>()
                        .map(|address| SocketAddr::new(address, ployz_core::WIREGUARD_PORT))
                })
                .map(AdvertisedEndpoint)
                .map_err(|_| Error::usage(format!("invalid WireGuard endpoint {value:?}")))
        })
        .collect()
}

pub(crate) fn command() -> Command {
    base("server", "Manage Servers")
        .arg_required_else_help(true)
        .subcommand(enroll::command())
        .subcommand(clean::command())
        .subcommand(base("build-cache-clear", "Clear this execution host user's Ployz build cache")
            .long_about("Clear this execution host user's Ployz build cache. Run on the build host as the user running its Builds (including the daemon). Refuses active or quarantined builder ownership; preserves completed images and unrelated Docker data. No daemon is required.\n\nHost configuration: ~/.ployz/build.yaml. Optional cpu_cores and memory_bytes limit BuildKit and Railpack preparation, independently of Service runtime limits. Both are disabled when omitted. Optional cache_bytes and min_free_bytes are retention/GC targets, not hard peak disk quotas. Unconfigured GC uses pinned BuildKit defaults."))
        .subcommand(
            base(
                "inspect",
                "Inspect a Server: telemetry, round-trip times, and its latest upgrade attempt",
            )
            .arg(positional("server", true)),
        )
        .subcommand(
            log_flags(base("logs", "Show Server daemon logs")).arg(Arg::new("service").num_args(0..).action(ArgAction::Append)),
        )
        .subcommand(base("ls", "List Servers"))
        .subcommand(
            base("rm", "Remove a Server")
                .long_about("Remove a Server from the Cluster and reset it. Type the Server's name with --confirm; without it the command fails with confirmation_required, naming what goes and the exact command to retry.")
                .arg(switch("no-reset", None).help(
                    "Remove the Server from the Cluster without resetting it; use when the Server is unreachable",
                ))
                .arg(value("confirm", None).value_name("SERVER").help("The Server's name, typed to confirm its removal"))
                .arg(positional("server", true))
                .arg(
                    volume_acceptance().conflicts_with("no-reset").help("Accept loss of Cluster access: repeat once per exact volume name; reset does not erase volume data on the host"),
                ),
        )
        .subcommand(
            machine_policy_flags(base("set", "Change a Server's name, labels, roles, public IP or build concurrency")
                .long_about("Change a Server's name, labels, roles, public IP or build concurrency. Changing --accepts-ingress also moves the Ingress Proxy onto or off that Server; rerun the same command to finish a move that failed."))
                .arg(many("label-rm", None).value_name("KEY"))
                .arg(value("name", None))
                .arg(value("public-ip", None).value_name("IP|none"))
                .arg(
                    value("build-concurrency", None)
                        .value_name("N|auto")
                        .help("Builds this Server runs at once; auto follows its roles and RAM"),
                )
                .arg(many("wg-endpoint", None))
                .arg(ingress_image())
                .arg(positional("server", true)),
        )
        .subcommand(
            base("upgrade", "Upgrade the daemon on explicitly selected Servers, one at a time")
                .long_about("Upgrade the daemon on explicitly selected Servers, one at a time, stopping at the first failure. When every daemon upgraded and any selected Server holds the ingress role, the Ingress Proxy then moves to the latest Caddy image on every Server with that role.")
                .arg_required_else_help(true)
                .arg(
                    positional("version", true)
                        .value_name("VERSION")
                        .value_parser(clap::value_parser!(ployz_core::MachineRelease)),
                )
                .arg(
                    positional("server", true)
                        .num_args(1..)
                        .action(ArgAction::Append)
                        .help("Server name or ID, in upgrade order"),
                )
                .arg(ingress_image()),
        )
}

/// Pin the Caddy image the Ingress Proxy is deployed with (offline test clusters).
fn ingress_image() -> Arg {
    value("ingress-image", None).value_name("IMAGE").hide(true)
}

pub(super) fn provisioning_flags(command: Command) -> Command {
    machine_policy_flags(command)
        .arg(value("name", None))
        .arg(switch("no-install", None))
        .arg(
            value("storage", None)
                .value_parser(clap::value_parser!(ployz_core::StorageChoice))
                .help(
                    "Enable Provisioned Volumes [Cloud default: zfs]; none keeps Docker Volumes only",
                ),
        )
        .arg(value("public-ip", None).default_value("auto"))
        .arg(
            value("ssh-key", Some('i'))
                .default_value("~/.ssh/id_ed25519")
                .value_hint(ValueHint::FilePath),
        )
        .arg(
            value("version", None)
                .env(env::DAEMON_VERSION)
                .default_value(env!("CARGO_PKG_VERSION"))
                .value_parser(clap::value_parser!(ployz_core::MachineRelease)),
        )
        .arg(many("wg-endpoint", None))
        .arg(value("wg-mtu", None).value_parser(clap::value_parser!(u32).range(1..)))
        .arg(switch("yes", Some('y')).env(env::AUTO_CONFIRM))
}

pub(super) fn handler(path: &str) -> Option<super::Handler> {
    Some(match path {
        "add" => enroll::add,
        "build-cache-clear" => clear_build_cache,
        "clean" => clean::clean,
        "inspect" => inspect,
        "logs" => super::operator::machine_logs,
        "ls" => list,
        "rm" => remove,
        "set" => set,
        "upgrade" => upgrade,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_accept_socket_or_ip_and_apply_the_default_port() {
        assert_eq!(
            parse_endpoints(&["192.0.2.1".into(), "[2001:db8::1]:6000".into()]).unwrap(),
            [
                AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap()),
                AdvertisedEndpoint("[2001:db8::1]:6000".parse().unwrap()),
            ]
        );
        assert!(parse_endpoints(&["not-an-address".into()]).is_err());
    }

    #[test]
    fn set_cli_rejects_an_empty_patch_and_maps_none_to_public_ip_removal() {
        let command = crate::cli::command();
        let empty = command
            .clone()
            .try_get_matches_from(["ployz", "server", "set", "node-a"])
            .unwrap();
        assert!(parse_update(leaf_matches(&empty)).is_err());

        let remove = command
            .try_get_matches_from(["ployz", "server", "set", "node-a", "--public-ip=none"])
            .unwrap();
        assert_eq!(
            parse_update(leaf_matches(&remove)).unwrap().public_ip,
            PublicIpUpdate::Remove
        );

        let concurrency = |value: &str| {
            let matches = crate::cli::command()
                .try_get_matches_from([
                    "ployz",
                    "server",
                    "set",
                    "node-a",
                    &format!("--build-concurrency={value}"),
                ])
                .unwrap();
            parse_update(leaf_matches(&matches)).map(|update| update.build_concurrency)
        };
        assert_eq!(
            concurrency("auto").unwrap(),
            BuildConcurrencyUpdate::Automatic
        );
        assert_eq!(
            concurrency("3").unwrap(),
            BuildConcurrencyUpdate::Set("3".parse().unwrap())
        );
        assert!(concurrency("0").is_err());
    }

    #[test]
    fn set_cli_validates_labels_and_preserves_independent_roles() {
        let parse = |flags: &[&str]| {
            let mut args = vec!["ployz", "server", "set", "node-a"];
            args.extend_from_slice(flags);
            let matches = crate::cli::command().try_get_matches_from(args).unwrap();
            parse_update(leaf_matches(&matches))
        };
        let patch = parse(&[
            "--label-add",
            "region=west",
            "--label-rm",
            "old",
            "--accepts-builds=true",
            "--accepts-services=false",
            "--accepts-ingress=true",
        ])
        .unwrap();
        assert_eq!(
            patch
                .label_changes
                .get("region")
                .and_then(Option::as_ref)
                .map(ployz_core::MachineLabelValue::as_str),
            Some("west")
        );
        assert_eq!(patch.label_changes.get("old"), Some(&None));
        assert_eq!(patch.accepts_builds, Some(true));
        assert_eq!(patch.accepts_services, Some(false));
        assert_eq!(patch.accepts_ingress, Some(true));
        for flags in [
            vec!["--label-add", "region"],
            vec!["--label-add", "=west"],
            vec!["--label-add", "rack/zone=west"],
            vec!["--label-add", "region="],
            vec!["--label-add", "region=é"],
            vec!["--label-add", "region=🦀"],
            vec!["--label-add", "region= west"],
            vec!["--label-add", "region=west "],
            vec!["--label-add", "region=west", "--label-add", "region=east"],
            vec!["--label-add", "region=west", "--label-rm", "region"],
        ] {
            assert!(parse(&flags).is_err(), "{flags:?}");
        }
    }

    #[test]
    fn singular_server_cli_target_rejects_star_and_keeps_all_as_identity() {
        let command = crate::cli::command();
        let star = command
            .clone()
            .try_get_matches_from(["ployz", "server", "set", "*", "--name", "edge"])
            .unwrap();
        assert!(MachineTarget::parse(target(leaf_matches(&star), "server").unwrap()).is_err());

        let named_all = command
            .try_get_matches_from(["ployz", "server", "set", "all", "--name", "edge"])
            .unwrap();
        assert_eq!(
            MachineTarget::parse(target(leaf_matches(&named_all), "server").unwrap())
                .unwrap()
                .as_str(),
            "all"
        );
    }

    fn cloud(reply: serde_json::Value) -> Credential {
        let cloud = crate::cloud_login::tests::fake_cloud(move |route, _| match route {
            "POST /api/cli/server-access" => (200, reply.clone()),
            other => panic!("unexpected {other}"),
        });
        Credential::Token {
            cloud,
            token: crate::cloud_login::Secret::new("ployz_secret".to_owned()),
        }
    }

    #[tokio::test]
    async fn cloud_access_without_servers_points_at_server_add() {
        let matches = crate::cli::command()
            .try_get_matches_from(["ployz", "server", "ls"])
            .unwrap();
        let credential = cloud(json!({ "connections": [], "unreachable": [] }));
        let Err(error) = through_cloud(leaf_matches(&matches), &credential).await else {
            panic!("dialed without a Server");
        };
        let error = error.report();
        assert_eq!(error.code, RpcErrorCode::NotFound);
        assert_eq!(error.details.get("next"), Some(&json!("ployz server add")));

        let offline = "b".repeat(32);
        let credential = cloud(json!({ "connections": [], "unreachable": [offline] }));
        let Err(error) = through_cloud(leaf_matches(&matches), &credential).await else {
            panic!("dialed without a Server");
        };
        let error = error.report();
        assert_eq!(error.code, RpcErrorCode::Unavailable);
        assert_eq!(error.details.get("unreachable"), Some(&json!([offline])));
    }
}
