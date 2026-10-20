use std::{
    collections::BTreeMap,
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
    PublicIpUpdate, RpcError, RpcErrorCode, SelectedEndpoint, UpdateMachineRequest, op,
};

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    cloud_account::{self, Credential, StoreCallError},
    cloud_login::{CredentialStore, LoginError},
    cluster::RoleWaitError,
    connect::{Client, ConnectError, SystemConnector, TARGET_RPC_TIMEOUT},
    context::{Config, ConnectionSource, ContextError, SelectedConnections},
    ingress::IngressImage,
};

use super::{Error, leaf_matches, string_values, with_client};
use crate::ui::Hint;

mod add;
pub(super) mod checkin;
pub(super) mod clean;
pub(super) mod drain;
mod endpoint;
pub(super) mod enroll;
pub(super) mod forget;
mod helpers;
mod init;
pub(super) mod reject;
mod remove;
mod show;
pub(super) mod token;
mod upgrade;
mod waiting;

use add::add;
pub(super) use enroll::{enroll_token, requested_storage};
pub(super) use helpers::{
    check_listed, confirm, connect_direct, enroll, inspect_token, leave, machine_name,
    readiness_timeout_message,
};
use init::init;
pub(super) use remove::remove;
pub(super) use show::{list, show};
pub(super) use upgrade::upgrade;

pub(super) fn clear_build_cache(matches: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(matches);
    if leaf.get_one::<String>("connect").is_some() || leaf.get_one::<String>("context").is_some() {
        return Err(Error::usage(
            "cache clearing runs on this execution host; run it there as the builder user without --connect or --context",
        ));
    }
    ployz_build::clear_cache(&ployz_build::HostPolicy::default()).map_err(|error| {
        Error::caused(
            ployz_core::RpcErrorCode::Internal,
            "Could not clear this host's build cache.",
            error,
        )
    })?;
    crate::ui::finish(&json!({ "build_cache": { "cleared": true } }), || {
        crate::ui::stream(format_args!("Cleared this host user's Ployz build cache."));
    })
}

/// Reach the Cluster for a live operation. `--connect` or `--context` win. Otherwise, signed in
/// to Cloud or holding `PLOYZ_TOKEN`, it dials through this credential's own Server Access;
/// signed out, through the current context or the local daemon.
pub(super) async fn connect(matches: &ArgMatches, context: Option<&str>) -> Result<Client, Error> {
    connect_or_none(matches, context)
        .await?
        .ok_or_else(no_servers)
}

/// [`connect`], or `None` when Cloud says this Organization has no Servers yet.
pub(super) async fn connect_or_none(
    matches: &ArgMatches,
    context: Option<&str>,
) -> Result<Option<Client>, Error> {
    if matches.get_one::<String>("connect").is_none() && context.is_none() {
        let store = CredentialStore::beside(&super::config_path(matches)?);
        match cloud_account::from_env(&store).await {
            Ok(credential) => {
                return dial(matches, cloud_account::server_access(&credential).await?).await;
            }
            Err(LoginError::SignedOut) => {
                // Signed out with no context or local daemon: the fix is signing in, not a
                // config file. The hidden test Store is never signed in, so it keeps the cause.
                return super::connect_context(matches, context)
                    .await
                    .map(Some)
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
    super::connect_context(matches, context).await.map(Some)
}

/// The credential Cloud runs a Server operation under: signed in, with neither `--connect`
/// nor `--context` naming a Cluster to dial, the same rule [`connect`] follows.
pub(super) fn cloud_runs(
    runtime: &tokio::runtime::Runtime,
    matches: &ArgMatches,
) -> Result<Option<Credential>, Error> {
    if matches.get_one::<String>("connect").is_some()
        || matches.get_one::<String>("context").is_some()
    {
        return Ok(None);
    }
    signed_in(runtime, matches)
}

/// The signed-in Cloud credential, or `None` when signed out. Nothing on the command line
/// or in the environment vetoes it.
pub(super) fn signed_in(
    runtime: &tokio::runtime::Runtime,
    matches: &ArgMatches,
) -> Result<Option<Credential>, Error> {
    let store = CredentialStore::beside(&super::config_path(matches)?);
    match runtime.block_on(cloud_account::from_env(&store)) {
        Ok(credential) => Ok(Some(credential)),
        Err(LoginError::SignedOut) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Whether [`signed_in`] would find something to act with, read from `PLOYZ_TOKEN` and the
/// stored sign-in alone: it never asks Cloud or writes, so a pending code counts as signed out.
pub(super) fn signed_in_locally(matches: &ArgMatches) -> bool {
    std::env::var(crate::cli::env::TOKEN).is_ok_and(|token| !token.is_empty())
        || super::config_path(matches).is_ok_and(|config| {
            matches!(CredentialStore::beside(&config).organization(), Ok(Some(_)))
        })
}

/// The Server `selector` names, for a Cloud route, among the Servers Cloud's dial sees.
/// An id resolves there too, since only the Engine knows the name prose says.
pub(super) fn cloud_machine(
    runtime: &tokio::runtime::Runtime,
    matches: &ArgMatches,
    selector: &str,
) -> Result<ployz_core::Machine, Error> {
    runtime.block_on(async {
        let mut client = connect(matches, None).await?;
        let machines = client.machines().await?;
        remove::select_machine(&machines, selector)
    })
}

/// `rerun` with `--approval id`, the command that goes ahead once a human approved.
pub(super) fn approved_rerun(matches: &ArgMatches, args: &[&str], id: &str) -> String {
    let mut args = args.to_vec();
    args.extend(["--approval", id]);
    super::rerun(matches, &args)
}

/// How a read reaches what it reads.
pub(super) enum Reach {
    /// Through the Servers themselves.
    Dial(Client),
    /// Through Cloud: this credential may read, but Cloud refused it Server access.
    Ask(Credential),
}

/// Reach the Cluster for a read Cloud can also answer, the way [`connect_or_none`] does,
/// except that a credential Cloud refuses Server access, such as a Read only or Deploy
/// token, comes back to ask Cloud instead.
pub(super) async fn reach(
    matches: &ArgMatches,
    context: Option<&str>,
) -> Result<Option<Reach>, Error> {
    if matches.get_one::<String>("connect").is_none() && context.is_none() {
        let store = CredentialStore::beside(&super::config_path(matches)?);
        if let Ok(credential) = cloud_account::from_env(&store).await {
            return match cloud_account::server_access(&credential).await {
                Ok(access) => Ok(dial(matches, access).await?.map(Reach::Dial)),
                Err(StoreCallError::Refused(error)) if error.code.as_str() == "forbidden" => {
                    Ok(Some(Reach::Ask(credential)))
                }
                Err(error) => Err(error.into()),
            };
        }
    }
    Ok(connect_or_none(matches, context).await?.map(Reach::Dial))
}

/// What a forget reaches.
pub(super) enum Reached {
    Client(Client),
    /// Through Cloud, whose Servers all failed to answer.
    Unanswered(Credential),
}

/// Reach the Cluster the way [`connect`] does, except that through Cloud a Cluster none of
/// whose Servers answers comes back with the credential, for Cloud to act alone.
pub(super) async fn connect_or_unanswered(
    matches: &ArgMatches,
    context: Option<&str>,
) -> Result<Reached, Error> {
    if matches.get_one::<String>("connect").is_none() && context.is_none() {
        let store = CredentialStore::beside(&super::config_path(matches)?);
        if let Ok(credential) = cloud_account::from_env(&store).await {
            let access = cloud_account::server_access(&credential).await?;
            return Ok(match answering(matches, access).await? {
                Some(client) => Reached::Client(client),
                None => Reached::Unanswered(credential),
            });
        }
    }
    connect(matches, context).await.map(Reached::Client)
}

/// Dial through Cloud's Server Access; `None` when Cloud knows Servers and none answers.
async fn answering(
    matches: &ArgMatches,
    mut access: cloud_account::ServerAccess,
) -> Result<Option<Client>, Error> {
    if access.connections.is_empty() {
        return match (access.unreachable.is_empty(), access.refused.is_empty()) {
            (true, true) => Err(no_servers()),
            (false, true) => Ok(None),
            (_, false) => Err(no_server_access(access, Vec::new(), None)),
        };
    }
    let held = held_servers(&access.connections);
    let refused = access.refused.is_empty();
    match dial_connections(matches, std::mem::take(&mut access.connections)).await {
        Ok(client) => Ok(Some(client)),
        Err(ConnectError::AllFailed { reached: false, .. }) if refused => Ok(None),
        Err(error @ ConnectError::AllFailed { reached: false, .. }) => {
            Err(no_server_access(access, held, Some(error)))
        }
        Err(error) => Err(error.into()),
    }
}

/// Every Server [`connect`] could enter by, in its order: Cloud's Server Access when
/// signed in, else the `--connect`, `--context` or local daemon connections.
async fn entries(
    matches: &ArgMatches,
    context: Option<&str>,
) -> Result<SelectedConnections, Error> {
    let direct = matches.get_one::<String>("connect").map(String::as_str);
    let mut signed_out = false;
    if direct.is_none() && context.is_none() {
        let store = CredentialStore::beside(&super::config_path(matches)?);
        match cloud_account::from_env(&store).await {
            Ok(credential) => {
                let access = cloud_account::server_access(&credential).await?;
                if access.connections.is_empty() {
                    return Err(
                        if access.unreachable.is_empty() && access.refused.is_empty() {
                            no_servers()
                        } else {
                            no_server_access(access, Vec::new(), None)
                        },
                    );
                }
                return Ok(SelectedConnections {
                    source: ConnectionSource::Cloud,
                    connections: access.connections,
                });
            }
            Err(LoginError::SignedOut) => signed_out = true,
            Err(error) => return Err(error.into()),
        }
    }
    crate::connect::resolve_connections(
        &super::config_path(matches)?,
        direct,
        context,
        std::path::Path::new(crate::connect::DEFAULT_LOCAL_SOCKET),
    )
    .map_err(|error| {
        let no_config = matches!(
            error,
            crate::connect::ConnectError::Context(ContextError::NoConfig)
        );
        if signed_out && no_config && !super::store::local_mode() {
            LoginError::SignedOut.into()
        } else {
            error.into()
        }
    })
}

/// Cloud's dial into this Organization's Servers, or `None` when it has none.
async fn dial(
    matches: &ArgMatches,
    mut access: cloud_account::ServerAccess,
) -> Result<Option<Client>, Error> {
    if access.connections.is_empty() {
        if access.unreachable.is_empty() && access.refused.is_empty() {
            return Ok(None);
        }
        return Err(no_server_access(access, Vec::new(), None));
    }
    let held = held_servers(&access.connections);
    match dial_connections(matches, std::mem::take(&mut access.connections)).await {
        Ok(client) => Ok(Some(client)),
        Err(error @ ConnectError::AllFailed { reached: false, .. })
            if !held.is_empty() || !access.unreachable.is_empty() || !access.refused.is_empty() =>
        {
            Err(no_server_access(access, held, Some(error)))
        }
        Err(error) => Err(error.into()),
    }
}

/// The Servers Cloud handed connections for, held without probing them.
fn held_servers(connections: &[crate::context::Connection]) -> Vec<ployz_core::MachineId> {
    connections
        .iter()
        .filter_map(|connection| connection.machine_id().copied())
        .collect()
}

async fn dial_connections(
    matches: &ArgMatches,
    connections: Vec<crate::context::Connection>,
) -> Result<Client, ConnectError> {
    let selected = SelectedConnections {
        source: ConnectionSource::Cloud,
        connections,
    };
    let connector = SystemConnector::default().with_ssh_timeout(crate::cli::ssh_timeout(matches));
    crate::connect::connect_selected_with(selected, Arc::new(connector)).await
}

const NO_SERVERS: &str = "this Organization has no Servers";

pub(super) fn no_servers() -> Error {
    Error::conflict(NO_SERVERS).hint(Hint::Next("ployz server add".into()))
}

/// Whether `report` is [`no_servers`]: Cloud knows no Server to look in.
fn has_no_servers(report: &RpcError) -> bool {
    report.code == RpcErrorCode::Conflict && report.message == NO_SERVERS
}

/// No Server of `access` gave this command access: the `unanswered` ones it held a
/// connection to and those Cloud couldn't reach didn't answer, and Cloud says why each
/// refused one couldn't. Only with no refusal does it read as [`no_reachable_server`].
fn no_server_access(
    access: cloud_account::ServerAccess,
    mut unanswered: Vec<ployz_core::MachineId>,
    cause: Option<ConnectError>,
) -> Error {
    unanswered.extend(access.unreachable);
    if access.refused.is_empty() {
        return no_reachable_server(unanswered, &access.names, cause);
    }
    let name = |id: &ployz_core::MachineId| {
        access
            .names
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.to_string())
    };
    let message = match (access.refused.as_slice(), unanswered.as_slice()) {
        ([one], []) => format!(
            "Server {} couldn't give this command access: {}",
            name(&one.machine_id),
            one.message
        ),
        (refused, unanswered) => {
            let mut why = refused
                .iter()
                .map(|server| format!("{} said \"{}\"", name(&server.machine_id), server.message))
                .collect::<Vec<_>>();
            why.extend(
                unanswered
                    .iter()
                    .map(|id| format!("{} isn't answering", name(id))),
            );
            format!(
                "No Server could give this command access: {}.",
                why.join("; ")
            )
        }
    };
    let refused = access
        .refused
        .iter()
        .map(|server| json!({ "machine_id": server.machine_id, "message": server.message }))
        .collect::<Vec<_>>();
    Error::detailed_caused(
        RpcErrorCode::Unavailable,
        message,
        json!({ "refused": refused, "unreachable": unanswered }),
        cause,
    )
}

/// None of `unreachable` answered, each named where Cloud holds its name; `cause` says how
/// the last attempt failed.
fn no_reachable_server(
    unreachable: Vec<ployz_core::MachineId>,
    names: &BTreeMap<ployz_core::MachineId, String>,
    cause: Option<ConnectError>,
) -> Error {
    let shown = unreachable
        .iter()
        .map(|id| names.get(id).cloned().unwrap_or_else(|| id.to_string()))
        .collect::<Vec<_>>();
    let message = match shown.as_slice() {
        [one] => format!(
            "Server {one} isn't answering. If it just joined or restarted, try again in a moment."
        ),
        _ => format!(
            "None of your Servers is answering: {}. If one just joined or restarted, try again in a moment.",
            super::joined(&shown)
        ),
    };
    Error::detailed_caused(
        RpcErrorCode::Unavailable,
        message,
        json!({ "unreachable": unreachable }),
        cause,
    )
}

/// Whether `report` is [`no_reachable_server`]'s: Servers Cloud knows that didn't answer,
/// and none that refused.
pub(super) fn servers_unanswered(report: &RpcError) -> bool {
    report.code == RpcErrorCode::Unavailable
        && report.details.get("unreachable").is_some()
        && report.details.get("refused").is_none()
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
/// `server show` shape, its Machine under `machine`.
pub(super) fn server_json(machine: &Machine) -> Value {
    json!({ "machine": machine_json(machine) })
}

pub(super) fn set(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let selector = target(matches, "server")?;
    let update = parse_update(matches)?;
    let image = matches.get_one::<String>("ingress-image");
    let mut args = vec!["server", "set", selector, "--accepts-ingress=true"];
    args.extend(
        image
            .iter()
            .flat_map(|image| ["--ingress-image", image.as_str()]),
    );
    let rerun = super::rerun(matches, &args);
    let ingress = IngressImage::given_or(image.cloned(), IngressImage::Keep);
    let selector = MachineTarget::parse(selector)?;
    let seed = update
        .advertised_endpoints
        .as_ref()
        .and_then(|endpoints| endpoints.first())
        .map(|endpoint| SelectedEndpoint(endpoint.0));
    let recovery = matches.clone();
    let Some(address) = seed else {
        return with_client(root, |client| {
            Box::pin(apply_update(
                client, selector, update, None, ingress, rerun, recovery,
            ))
        });
    };
    super::runtime()?.block_on(async {
        let context = matches.get_one::<String>("context").map(String::as_str);
        let entries = entries(matches, context).await?;
        let connector =
            SystemConnector::default().with_ssh_timeout(crate::cli::ssh_timeout(matches));
        let mut seeding = endpoint::seed(&entries, Arc::new(connector), &selector, address).await;
        if let Some(sentence) = seeding.sentence() {
            crate::ui::stream(sentence);
        }
        let Some(mut client) = seeding.updater.take() else {
            return Err(unseeded(&seeding));
        };
        for missed in &seeding.failures {
            crate::ui::warn(missed.sentence());
        }
        apply_update(
            &mut client,
            selector,
            update,
            Some(seeding),
            ingress,
            rerun,
            recovery,
        )
        .await
    })
}

/// No Server took the endpoint and none was reached to update the target: the first failure.
fn unseeded(seeding: &endpoint::Seeding) -> Error {
    seeding.failures.first().map_or_else(
        || Error::not_found("no Server took the endpoint"),
        |missed| {
            let what = if missed.error.code == RpcErrorCode::Unavailable {
                "did not answer"
            } else {
                "refused the endpoint"
            };
            Error::from(missed.error.clone()).context(format!("{} {what}.", missed.entry))
        },
    )
}

/// Update the target's record. With `seeding`, a target that cannot be reached still
/// leaves the other Servers dialing it, and the command succeeds.
async fn apply_update(
    client: &mut Client,
    selector: MachineTarget,
    update: MachineUpdate,
    seeding: Option<endpoint::Seeding>,
    ingress: IngressImage,
    rerun: String,
    recovery: ArgMatches,
) -> Result<(), Error> {
    let accepts_ingress = update.accepts_ingress;
    let updated = client
        .invoke_answered::<op::UpdateMachine>(
            UpdateMachineRequest { update },
            &selector,
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map_err(|error| (error.unreplied(), RpcError::from(error)));
    let machine = match (updated, &seeding) {
        (Ok(updated), _) => updated.machine,
        (Err((unreplied, error)), Some(seeding))
            if error.code == RpcErrorCode::Unavailable && !seeding.dialed_by.is_empty() =>
        {
            let name = seeding.target.as_ref().map_or_else(
                || selector.as_str().to_owned(),
                |target| target.name.to_string(),
            );
            crate::ui::warn(if unreplied {
                format!("{name} didn't answer, so its record may or may not have changed.")
            } else {
                format!("{name} couldn't take the change; its record is unchanged.")
            });
            return crate::ui::emit(&json!({
                "server": null,
                "endpoint": seeding,
                "target_error": error,
            }));
        }
        (Err((true, error)), _) => {
            return Err(Error::from(error).context(format!(
                "Server {selector} didn't answer, so the change may or may not have landed. Check it with ployz server ls once it's back, or run this on the Server."
            )));
        }
        (Err((false, error)), _) if error.code == RpcErrorCode::Unavailable => {
            return Err(Error::from(error)
                .context(format!(
                    "Server {selector} answered but couldn't take the change, so nothing changed."
                ))
                .hint(Hint::Retry(super::typed_with(&[]))));
        }
        (Err((_, error)), _) => return Err(error.into()),
    };
    crate::ui::stream(format_args!("Updated Server {}.", machine.name));
    // The update is committed; starting the Ingress Proxy for a new ingress role is a
    // follow-up. Turning the role off leaves the proxy serving: Hosted DNS stops
    // advertising the Server at its next sync, and `server rm` stops the proxy.
    if accepts_ingress != Some(true) {
        return crate::ui::emit(&json!({ "server": server_json(&machine), "endpoint": seeding }));
    }
    let followed = async {
        crate::cluster::wait_for_role(
            client,
            &machine.id,
            crate::cluster::RoleSetting::IngressOn,
            &CancellationToken::new(),
        )
        .await
        .map_err(|error| match error {
            RoleWaitError::Connect(error) => Error::from(error),
            error @ (RoleWaitError::NotObserved(_) | RoleWaitError::Cancelled) => {
                Error::from(error)
            }
        })?;
        crate::ingress::follow_roles(client, ingress).await
    }
    .await;
    crate::ui::emit_committed(
        json!({
            "server": server_json(&machine),
            "endpoint": seeding,
            "ingress": followed.as_ref().ok().and_then(Option::as_ref),
        }),
        followed.map(drop).map_err(|error| {
            ingress_incomplete(
                "Server updated",
                super::ingress_hints(error, |args| super::rerun(&recovery, args)),
                rerun,
            )
        }),
    )
}

/// A committed Server change whose Ingress Proxy follow-up failed: name it and the exact rerun.
pub(super) fn ingress_incomplete(committed: &str, error: Error, next: String) -> Error {
    error
        .context(format!("{committed}; the Ingress Proxy did not follow."))
        .hint(Hint::Next(next))
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

pub(super) fn group(command: Command) -> Command {
    base(command, "Manage Servers")
}

pub(super) fn clear_build_cache_command(command: Command) -> Command {
    base(command, "Clear this execution host user's Ployz build cache")
            .long_about("Clear this execution host user's Ployz build cache. Run on the build host as the user running its Builds (including the daemon). Refuses active or quarantined builder ownership; preserves completed images and unrelated Docker data. No daemon is required.\n\nHost configuration: ~/.ployz/build.yaml. Optional cpu_cores and memory_bytes limit BuildKit and Railpack preparation, independently of Service runtime limits. Both are disabled when omitted. Optional cache_bytes and min_free_bytes are retention/GC targets, not hard peak disk quotas. Unconfigured GC uses pinned BuildKit defaults.")
}

pub(super) fn drain_command(command: Command) -> Command {
    base(command, "Stop placing Services on a Server and move its Services' Containers off it")
                .long_about("Turn off the Server's services role, then move each replicated Service's Containers off it one at a time: a new Container starts on another Server from the same image and serves before the old one stops. No hooks run and no Deployment is recorded. Rerun to act on what remains. Turning the services role back on does not move anything back. Signed in to Cloud without --context or --connect, Cloud runs the drain, asking a human first when the Organization wants that.")
                .arg(positional("server", true))
                .arg(crate::cli::approval())
}

pub(super) fn show_command(command: Command) -> Command {
    base(
        command,
        "Show a Server: telemetry, round-trip times and its latest upgrade attempt, or a server waiting to join",
    )
    .arg(positional("server", true))
}

pub(super) fn machine_logs_command(command: Command) -> Command {
    log_flags(base(command, "Show Server daemon logs"))
        .arg(Arg::new("service").num_args(0..).action(ArgAction::Append))
}

pub(super) fn list_command(command: Command) -> Command {
    base(command, "List Servers")
        .long_about(
            "List the Cluster's Servers. With --waiting, list the servers waiting to join \
             your Organization in Ployz Cloud instead: name, provider, IP, status and age.",
        )
        .arg(switch("waiting", None).help("List servers waiting to join instead"))
}

pub(super) fn remove_command(command: Command) -> Command {
    base(command, "Remove a Server")
                .long_about("Remove a Server from the Cluster and reset it; for a Server that is gone or can't be reset, use ployz server forget. Type the Server's name with --confirm, or in a terminal when it asks; elsewhere it fails with confirmation_required, naming what goes and the exact command to retry. When Cloud manages the Server, Cloud removes it even with --context or --connect, asking a human first when the Organization wants that.")
                .arg(value("confirm", None).value_name("SERVER").help("The Server's name, typed to confirm its removal"))
                .arg(positional("server", true))
                .arg(crate::cli::approval())
                .arg(
                    volume_acceptance().help("Accept loss of Cluster access: repeat once per exact volume name; reset does not erase volume data on the host"),
                )
}

pub(super) fn set_command(command: Command) -> Command {
    machine_policy_flags(base(command, "Change a Server's name, labels, roles, public IP or build concurrency")
                .long_about("Change a Server's name, labels, roles, public IP or build concurrency. --accepts-ingress=true also starts the Ingress Proxy on that Server; rerun the same command to finish a start that failed. --accepts-ingress=false stops advertising the Server in Hosted DNS from the next sync, but its Ingress Proxy keeps serving until `ployz server rm`."))
                .arg(many("label-rm", None).value_name("KEY"))
                .arg(value("name", None))
                .arg(value("public-ip", None).value_name("IP|none"))
                .arg(
                    value("build-concurrency", None)
                        .value_name("N|auto")
                        .help("Builds this Server runs at once; auto follows its roles and RAM"),
                )
                .arg(many("wg-endpoint", None).value_name("IP[:PORT]").help(
                    "Where peers reach this Server over WireGuard; every other Server the command reaches dials the first one now, even if this Server is unreachable",
                ))
                .arg(ingress_image())
                .arg(positional("server", true))
}

pub(super) fn upgrade_command(command: Command) -> Command {
    base(command, "Upgrade the daemon on explicitly selected Servers, one at a time")
                .long_about("Upgrade the daemon on explicitly selected Servers, one at a time, stopping at the first failure. When every daemon upgraded and any selected Server holds the ingress role, the Ingress Proxy then moves to the latest Caddy image on every Server with that role. Signed in to Cloud without --context or --connect, Cloud runs each upgrade along the Organization's Release Channel; an exact version needs --context.")
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
                .arg(ingress_image())
}

/// Pin the Caddy image the Ingress Proxy is deployed with (offline test clusters).
fn ingress_image() -> Arg {
    value("ingress-image", None)
        .value_name("IMAGE")
        .value_parser(crate::ingress::pinned_image)
        .hide(true)
}

pub(super) fn provisioning_flags(command: Command) -> Command {
    machine_policy_flags(command)
        .arg(
            value("name", None)
                .help("Name the Server; it takes its hostname when omitted"),
        )
        .arg(switch("no-install", None))
        .arg(
            value("storage", None)
                .value_parser(clap::value_parser!(ployz_core::StoragePreparation))
                .help(
                    "zfs sets up Managed volumes [Cloud default]; zfs:<device> uses an empty disk; none is Docker only",
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
    fn enrolling_pins_only_the_addresses_the_operator_names() {
        let pinned = |flags: &[&str]| {
            let matches = crate::cli::command()
                .try_get_matches_from(
                    ["ployz", "server", "add", "--standalone", "root@host"]
                        .iter()
                        .chain(flags),
                )
                .unwrap();
            helpers::pinned(&helpers::token_request(leaf_matches(&matches)).unwrap())
        };
        let pins = |public_ip, advertised_endpoints| ployz_core::PinnedAddresses {
            public_ip,
            advertised_endpoints,
        };
        assert_eq!(pinned(&[]), pins(false, false));
        assert_eq!(pinned(&["--public-ip=auto"]), pins(false, false));
        assert_eq!(pinned(&["--public-ip=none"]), pins(true, false));
        assert_eq!(pinned(&["--public-ip=192.0.2.9"]), pins(true, false));
        assert_eq!(pinned(&["--wg-endpoint=192.0.2.1"]), pins(false, true));
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

    #[tokio::test]
    async fn cloud_access_dials_nothing_without_servers_and_names_unreachable_ones() {
        let matches = crate::cli::command()
            .try_get_matches_from(["ployz", "server", "ls"])
            .unwrap();
        let access = |unreachable| cloud_account::ServerAccess {
            connections: Vec::new(),
            unreachable,
            refused: Vec::new(),
            names: [(
                ployz_core::MachineId::parse("b".repeat(32)).unwrap(),
                "box-node-7".to_owned(),
            )]
            .into(),
        };
        let dialed = dial(leaf_matches(&matches), access(Vec::new())).await;
        assert!(dialed.unwrap().is_none());

        let offline = ployz_core::MachineId::parse("b".repeat(32)).unwrap();
        let Err(error) = dial(leaf_matches(&matches), access(vec![offline])).await else {
            panic!("dialed without a Server");
        };
        let error = error.report();
        assert_eq!(error.code, RpcErrorCode::Unavailable);
        assert_eq!(
            error.message,
            "Server box-node-7 isn't answering. If it just joined or restarted, try again in a moment."
        );
        assert_eq!(
            error.details.get("unreachable"),
            Some(&json!(["b".repeat(32)]))
        );
    }

    #[tokio::test]
    async fn held_connections_that_reach_no_server_name_the_unreachable_servers() {
        let matches = crate::cli::command()
            .try_get_matches_from(["ployz", "server", "ls"])
            .unwrap();
        let (held, gone) = ("a".repeat(32), "b".repeat(32));
        let access = cloud_account::ServerAccess {
            connections: serde_json::from_value(json!([
                { "machine_id": held, "unix": "/nonexistent/ployz-test/daemon.sock" }
            ]))
            .unwrap(),
            unreachable: vec![ployz_core::MachineId::parse(gone.clone()).unwrap()],
            refused: Vec::new(),
            names: [(
                ployz_core::MachineId::parse(held.clone()).unwrap(),
                "web-1".to_owned(),
            )]
            .into(),
        };
        let Err(error) = dial(leaf_matches(&matches), access).await else {
            panic!("dialed a Server that is not there");
        };
        assert!(
            !error.causes().is_empty(),
            "the connect error stays the cause"
        );
        let error = error.report();
        assert_eq!(error.code, RpcErrorCode::Unavailable);
        assert_eq!(
            error.message,
            format!(
                "None of your Servers is answering: web-1, {gone}. If one just joined or restarted, try again in a moment."
            )
        );
        assert_eq!(error.details.get("unreachable"), Some(&json!([held, gone])));
    }

    #[tokio::test]
    async fn a_cluster_whose_servers_all_fail_to_answer_comes_back_unanswered() {
        let matches = crate::cli::command()
            .try_get_matches_from(["ployz", "server", "forget", "web-1"])
            .unwrap();
        let matches = leaf_matches(&matches);
        let unprovisioned = cloud_account::ServerAccess {
            connections: Vec::new(),
            unreachable: vec![ployz_core::MachineId::parse("b".repeat(32)).unwrap()],
            refused: Vec::new(),
            names: BTreeMap::new(),
        };
        assert!(answering(matches, unprovisioned).await.unwrap().is_none());

        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let held: cloud_account::ServerAccess = serde_json::from_value(json!({
            "connections": [{ "machine_id": "a".repeat(32), "tcp": closed.to_string() }],
            "unreachable": [],
        }))
        .unwrap();
        assert!(answering(matches, held).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_server_that_refused_cloud_is_not_one_that_did_not_answer() {
        let matches = crate::cli::command()
            .try_get_matches_from(["ployz", "server", "forget", "web-1"])
            .unwrap();
        let matches = leaf_matches(&matches);
        let (refusing, gone) = ("a".repeat(32), "b".repeat(32));
        let access = |unreachable: &[&String]| -> cloud_account::ServerAccess {
            serde_json::from_value(json!({
                "connections": [],
                "unreachable": unreachable,
                "refused": [{ "machine_id": refusing, "message": "Cluster store is not available" }],
                "names": { refusing.clone(): "web-1" },
            }))
            .unwrap()
        };

        let Err(error) = dial(matches, access(&[])).await else {
            panic!("dialed a Server that refused");
        };
        let report = error.report();
        assert_eq!(
            report.message,
            "Server web-1 couldn't give this command access: Cluster store is not available"
        );
        assert!(!servers_unanswered(&report), "{report:?}");
        let Err(error) = answering(matches, access(&[])).await else {
            panic!("a Server that refused is not unanswered");
        };
        assert!(!servers_unanswered(&error.report()));

        let Err(error) = dial(matches, access(&[&gone])).await else {
            panic!("dialed a Server that refused");
        };
        let report = error.report();
        assert_eq!(
            report.message,
            format!(
                "No Server could give this command access: web-1 said \"Cluster store is not available\"; {gone} isn't answering."
            )
        );
        assert_eq!(report.details.get("unreachable"), Some(&json!([gone])));
        assert!(!servers_unanswered(&report), "{report:?}");
    }
}
