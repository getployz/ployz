//! Where a command that reaches a Cluster goes: the credential read once, the order its
//! routes are tried in, and why no Server could carry it.

use std::{collections::BTreeMap, sync::Arc};

use clap::ArgMatches;
use ployz_core::{Hint, Machine, RpcErrorCode, op};
use serde_json::json;

use super::Error;
use crate::{
    cloud_account::{self, Credential, StoreCallError},
    cloud_login::{CredentialStore, LoginError},
    cluster::{Call, FullRoutes},
    connect::{Client, ConnectError, Connector, SystemConnector},
    context::{Connection, ConnectionSource, ContextError, SelectedConnections},
    failure::ConnectFailure,
};

/// Reach the Cluster for a live operation. `--connect` or `--context` win. Otherwise, signed in
/// to Cloud or holding `PLOYZ_TOKEN`, it dials through this credential's own Server Access;
/// signed out, through the current context or the local daemon.
pub(super) async fn connect(matches: &ArgMatches, context: Option<&str>) -> Result<Client, Error> {
    connect_or_none(matches, context)
        .await?
        .ok_or_else(no_servers)
}

/// [`connect`] for what needs Full scope: exec, tunnels and the Device roster. The
/// `--connect`, `--context` or current context, or the local daemon, goes first, so over SSH
/// on a Server that approves the first Device; signed in, Cloud's Server Access follows.
/// Only a Device slot grants Full, so with a Device key every Management Capability
/// connection dials as the Device. Several `--connect` reach each Server over its own, as
/// [`connect_each`] says.
pub(super) async fn connect_full(
    matches: &ArgMatches,
    context: Option<&str>,
) -> Result<Client, Error> {
    let config = super::config_path(matches)?;
    let key = super::device::key::path(&config);
    let direct: Vec<&str> = matches
        .get_many::<String>("connect")
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
    if let [_, _, ..] = direct[..] {
        let connections = direct
            .iter()
            .map(|direct| direct.parse())
            .collect::<Result<Vec<Connection>, _>>()
            .map_err(ConnectError::from)?;
        let connector =
            SystemConnector::default().with_ssh_timeout(crate::cli::ssh_timeout(matches));
        return connect_each(connections, Arc::new(connector)).await;
    }
    let direct = direct.first().copied();
    let access = access(matches, context).await?;
    let local = match crate::connect::resolve_connections(
        &config,
        direct,
        context,
        &ployz_core::MachineLayout::production().socket(),
    ) {
        Ok(mut selected) => {
            selected.connections = as_device(selected.connections, &key)?;
            let connector =
                SystemConnector::default().with_ssh_timeout(crate::cli::ssh_timeout(matches));
            let connections = selected.connections.clone();
            crate::connect::connect_selected_with(selected, Arc::new(connector))
                .await
                .map(|client| {
                    client.with_full_routes(FullRoutes::chosen(connections, direct.is_some()))
                })
        }
        Err(error) => Err(error),
    };
    let Access::Cloud(credential) = access else {
        let signed_out = matches!(access, Access::Direct { signed_out: true });
        return local.map_err(|error| signed_out_unless_configured(error.into(), signed_out));
    };
    if let Ok(client) = local {
        return Ok(client);
    }
    let mut access = cloud_account::server_access(&credential).await?;
    access.connections = as_device(access.connections, &key)?;
    let connections = access.connections.clone();
    dial(matches, access)
        .await?
        .map(|client| {
            client.with_full_routes(FullRoutes::from_cloud(connections, direct.is_some()))
        })
        .ok_or_else(no_servers)
}

/// A client entering by the first of `connections` that holds each one as the route the
/// person chose to the Server answering on it. Each Server names itself over its own
/// connection, so no route is labelled from Cloud's or another Server's word.
///
/// # Errors
/// Fails when a connection doesn't answer, or two reach the same Server.
pub(super) async fn connect_each(
    connections: Vec<Connection>,
    connector: Arc<dyn Connector>,
) -> Result<Client, Error> {
    let dialed = futures_util::future::try_join_all(connections.into_iter().map(|connection| {
        let connector = Arc::clone(&connector);
        async move {
            let selected = SelectedConnections {
                source: ConnectionSource::Direct,
                connections: vec![connection.clone()],
            };
            let dial = async {
                let client = crate::connect::connect_selected_with(selected, connector).await?;
                let machine = client.entry_id().await?;
                Ok::<_, ConnectError>((client, machine))
            };
            let (client, machine) = dial.await.map_err(|error| {
                Error::from(error).context(format!("No Server answered at {connection}."))
            })?;
            Ok::<_, Error>((client, connection.with_machine_id(machine)))
        }
    }))
    .await?;
    let mut routes: Vec<Connection> = Vec::new();
    for (_, connection) in &dialed {
        if let Some(twice) = routes
            .iter()
            .find(|route| route.machine_id() == connection.machine_id())
        {
            return Err(Error::usage(format!(
                "{twice} and {connection} reach the same Server. Give each Server's connection once."
            )));
        }
        routes.push(connection.clone());
    }
    let (client, _) = dialed.into_iter().next().ok_or_else(no_servers)?;
    Ok(client.with_full_routes(FullRoutes::chosen(routes, false)))
}

/// `connections`, each Management Capability one dialing as the Device whose key is at `key`
/// when there is one. SSH and the local socket never read the key.
fn as_device(
    connections: Vec<Connection>,
    key: &std::path::Path,
) -> Result<Vec<Connection>, super::device::key::KeyError> {
    let management = |connection: &Connection| {
        matches!(
            connection.transport(),
            crate::context::Transport::Management(_)
        )
    };
    if !connections.iter().any(management) {
        return Ok(connections);
    }
    Ok(match super::device::key::load(key)? {
        Some(secret) => connections
            .into_iter()
            .map(|connection| connection.with_device(secret))
            .collect(),
        None => connections,
    })
}

/// Signed out with no context or local daemon: the fix is signing in, not a config file.
/// The hidden test Store is never signed in, so it keeps the cause.
fn signed_out_unless_configured(error: Error, signed_out: bool) -> Error {
    let no_config = matches!(error.context_error(), Some(ContextError::NoConfig));
    if signed_out && no_config && !super::store::local_mode() {
        LoginError::SignedOut.into()
    } else {
        error
    }
}

/// The credential, read once: what a route is chosen by.
enum Login {
    SignedIn(Credential),
    SignedOut,
    Broken(LoginError),
}

async fn login(matches: &ArgMatches) -> Result<Login, Error> {
    let store = CredentialStore::beside(&super::config_path(matches)?);
    Ok(match cloud_account::from_env(&store).await {
        Ok(credential) => Login::SignedIn(credential),
        Err(LoginError::SignedOut) => Login::SignedOut,
        Err(error) => Login::Broken(error),
    })
}

/// How a command enters the Cluster. `--connect` or `--context` name a Cluster to dial and
/// no credential is read. Otherwise a credential Cloud accepts dials through its Server
/// Access, and signed out the command dials directly. A login that is broken stops every
/// route, since no route can tell whether it was meant to be used.
enum Access {
    Cloud(Credential),
    Direct { signed_out: bool },
}

async fn access(matches: &ArgMatches, context: Option<&str>) -> Result<Access, Error> {
    if matches.get_one::<String>("connect").is_some() || context.is_some() {
        return Ok(Access::Direct { signed_out: false });
    }
    match login(matches).await? {
        Login::SignedIn(credential) => Ok(Access::Cloud(credential)),
        Login::SignedOut => Ok(Access::Direct { signed_out: true }),
        Login::Broken(error) => Err(error.into()),
    }
}

/// [`connect`], or `None` when Cloud says this Organization has no Servers yet.
pub(super) async fn connect_or_none(
    matches: &ArgMatches,
    context: Option<&str>,
) -> Result<Option<Client>, Error> {
    Ok(connect_or_none_held(matches, context)
        .await?
        .map(|(client, _)| client))
}

/// [`connect`], with the connections Cloud handed over when it was Cloud that gave them;
/// `None` for those of `--connect`, a context or the local socket, which cost nothing to read
/// again.
pub(super) async fn connect_held(
    matches: &ArgMatches,
    context: Option<&str>,
) -> Result<(Client, Option<Vec<Connection>>), Error> {
    connect_or_none_held(matches, context)
        .await?
        .ok_or_else(no_servers)
}

async fn connect_or_none_held(
    matches: &ArgMatches,
    context: Option<&str>,
) -> Result<Option<(Client, Option<Vec<Connection>>)>, Error> {
    match access(matches, context).await? {
        Access::Cloud(credential) => {
            let access = cloud_account::server_access(&credential).await?;
            Ok(dial_held(matches, access)
                .await?
                .map(|(client, held)| (client, Some(held))))
        }
        Access::Direct { signed_out } => Ok(Some((
            direct_client(matches, context, signed_out).await?,
            None,
        ))),
    }
}

/// The client for `--connect`, `--context`, the current context or the local daemon.
async fn direct_client(
    matches: &ArgMatches,
    context: Option<&str>,
    signed_out: bool,
) -> Result<Client, Error> {
    super::connect_context(matches, context)
        .await
        .map_err(|error| signed_out_unless_configured(error, signed_out))
}

/// [`connect`] through any Server but `leaving`, and the Server it entered by.
pub(super) async fn connect_avoiding(
    matches: &ArgMatches,
    context: Option<&str>,
    leaving: &Machine,
) -> Result<(Client, ployz_core::MachineId), Error> {
    let mut selected = entries(matches, context).await?;
    let via = selected.source.clone();
    selected
        .connections
        .retain(|connection| connection.machine_id() != Some(&leaving.id));
    if !selected.connections.is_empty() {
        let connector =
            SystemConnector::default().with_ssh_timeout(crate::cli::ssh_timeout(matches));
        let mut client =
            crate::connect::connect_selected_with(selected, Arc::new(connector)).await?;
        let entry = client
            .call::<op::DescribeContract>(ployz_core::DescribeContractRequest {}, Call::read())
            .await?
            .machine_id;
        if entry != leaving.id {
            return Ok((client, entry));
        }
    }
    Err(Error::conflict(format!(
        "Server {} is the only Server reachable through {via}, so it cannot carry its own removal. \
         Connect through another Server, then retry. No changes made.",
        leaving.name
    )))
}

/// [`connect_full`], entered through a Server other than `leaving` while another one is
/// listed, so the entry can carry the removal. The client keeps every connection, the
/// leaving Server's too, so a Leave still dials it directly. Also returns the entry and
/// whether `leaving` had to be avoided.
pub(super) async fn connect_full_avoiding(
    matches: &ArgMatches,
    context: Option<&str>,
    leaving: Option<&Machine>,
) -> Result<(Client, ployz_core::MachineId, bool), Error> {
    let mut client = connect_full(matches, context).await?;
    let entry = client
        .call::<op::DescribeContract>(ployz_core::DescribeContractRequest {}, Call::read())
        .await?
        .machine_id;
    let Some(leaving) = leaving.filter(|leaving| leaving.id == entry) else {
        return Ok((client, entry, false));
    };
    let routes = client.full_routes().spread();
    let every = routes.connections().to_vec();
    let others: Vec<Connection> = every
        .iter()
        .filter(|connection| connection.machine_id().is_some_and(|id| *id != leaving.id))
        .cloned()
        .collect();
    let via = client.connection_source().clone();
    if !others.is_empty() {
        let connector =
            SystemConnector::default().with_ssh_timeout(crate::cli::ssh_timeout(matches));
        let selected = SelectedConnections {
            source: via.clone(),
            connections: others,
        };
        let mut client = crate::connect::connect_selected_with(selected, Arc::new(connector))
            .await?
            .with_full_routes(routes);
        let entry = client
            .call::<op::DescribeContract>(ployz_core::DescribeContractRequest {}, Call::read())
            .await?
            .machine_id;
        if entry != leaving.id {
            return Ok((client, entry, true));
        }
    }
    Err(Error::conflict(format!(
        "Server {} is the only Server reachable through {via}, so it cannot carry its own removal. \
         Connect through another Server, then retry. No changes made.",
        leaving.name
    )))
}

/// [`connect`] through `machine` alone, or `None` when no connection names it, as with a
/// local daemon socket or a context that lists only other Servers. `held` are the
/// connections Cloud already handed over, so it isn't asked again.
pub(super) async fn connect_only(
    matches: &ArgMatches,
    context: Option<&str>,
    held: Option<&[Connection]>,
    machine: &ployz_core::MachineId,
) -> Result<Option<Client>, Error> {
    let mut selected = match held {
        Some(connections) => SelectedConnections {
            source: ConnectionSource::Cloud,
            connections: connections.to_vec(),
        },
        None => entries(matches, context).await?,
    };
    selected
        .connections
        .retain(|connection| connection.machine_id() == Some(machine));
    if selected.connections.is_empty() {
        return Ok(None);
    }
    let connector = SystemConnector::default().with_ssh_timeout(crate::cli::ssh_timeout(matches));
    Ok(Some(
        crate::connect::connect_selected_with(selected, Arc::new(connector)).await?,
    ))
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
    match runtime.block_on(login(matches))? {
        Login::SignedIn(credential) => Ok(Some(credential)),
        Login::SignedOut => Ok(None),
        Login::Broken(error) => Err(error.into()),
    }
}

/// Whether [`signed_in`] would find something to act with, read from `PLOYZ_TOKEN` and the
/// stored sign-in alone: it never asks Cloud or writes, so a pending code counts as signed out.
pub(super) fn signed_in_locally(matches: &ArgMatches) -> bool {
    crate::cloud_account::env_token().is_some()
        || super::config_path(matches).is_ok_and(|config| {
            matches!(CredentialStore::beside(&config).organization(), Ok(Some(_)))
        })
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
    reach_or_silent(matches, context)
        .await
        .map_err(|unreached| unreached.error)
}

/// Why [`reach_or_silent`] reached no Server, and whether that is because none answered.
pub(super) struct Unreached {
    pub(super) error: Error,
    pub(super) silent: Option<Silent>,
}

impl From<Error> for Unreached {
    fn from(error: Error) -> Self {
        Self {
            error,
            silent: None,
        }
    }
}

/// [`reach`], telling a connection no Server answered apart from other failures. A route
/// that enters by a Unix socket is never [`Silent`]: that is this host's own daemon, which
/// `status` reports as this Server.
pub(super) async fn reach_or_silent(
    matches: &ArgMatches,
    context: Option<&str>,
) -> Result<Option<Reach>, Unreached> {
    let direct = matches.get_one::<String>("connect").map(String::as_str);
    let config = super::config_path(matches)?;
    let signed_out = match access(matches, context).await? {
        Access::Cloud(credential) => {
            let access = match cloud_account::server_access(&credential).await {
                Ok(access) => access,
                Err(StoreCallError::Refused(error))
                    if error.code.as_str() == ployz_core::FORBIDDEN_CODE =>
                {
                    return Ok(Some(Reach::Ask(credential)));
                }
                Err(error) => return Err(Error::from(error).into()),
            };
            let socket = by_socket(&access.connections);
            return match dial(matches, access).await {
                Ok(client) => Ok(client.map(Reach::Dial)),
                Err(error) => Err(Unreached::of(error, socket)),
            };
        }
        Access::Direct { signed_out } => signed_out,
    };
    let socket = crate::connect::resolve_connections(
        &config,
        direct,
        context,
        &ployz_core::MachineLayout::production().socket(),
    )
    .is_ok_and(|selected| by_socket(&selected.connections));
    match direct_client(matches, context, signed_out).await {
        Ok(client) => Ok(Some(Reach::Dial(client))),
        Err(error) => Err(Unreached::of(error, socket)),
    }
}

impl Unreached {
    fn of(error: Error, socket: bool) -> Self {
        let silent = if socket { None } else { Silent::of(&error) };
        Self { error, silent }
    }
}

fn by_socket(connections: &[crate::context::Connection]) -> bool {
    connections
        .iter()
        .any(|connection| matches!(connection.transport(), crate::context::Transport::Unix(_)))
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
    match access(matches, context).await? {
        Access::Cloud(credential) => {
            let access = cloud_account::server_access(&credential).await?;
            Ok(match answering(matches, access).await? {
                Some(client) => Reached::Client(client),
                None => Reached::Unanswered(credential),
            })
        }
        Access::Direct { signed_out } => direct_client(matches, context, signed_out)
            .await
            .map(Reached::Client),
    }
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
    let connections = named(std::mem::take(&mut access.connections), &access.names);
    match dial_connections(matches, connections).await {
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
pub(super) async fn entries(
    matches: &ArgMatches,
    context: Option<&str>,
) -> Result<SelectedConnections, Error> {
    let direct = matches.get_one::<String>("connect").map(String::as_str);
    let signed_out = match access(matches, context).await? {
        Access::Cloud(credential) => {
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
                connections: named(access.connections, &access.names),
            });
        }
        Access::Direct { signed_out } => signed_out,
    };
    crate::connect::resolve_connections(
        &super::config_path(matches)?,
        direct,
        context,
        &ployz_core::MachineLayout::production().socket(),
    )
    .map_err(|error| signed_out_unless_configured(error.into(), signed_out))
}

/// Cloud's dial into this Organization's Servers, or `None` when it has none.
async fn dial(
    matches: &ArgMatches,
    access: cloud_account::ServerAccess,
) -> Result<Option<Client>, Error> {
    Ok(dial_held(matches, access).await?.map(|(client, _)| client))
}

/// [`dial`], with the connections Cloud handed over, so a later dial needs no second ask.
async fn dial_held(
    matches: &ArgMatches,
    mut access: cloud_account::ServerAccess,
) -> Result<Option<(Client, Vec<Connection>)>, Error> {
    if access.connections.is_empty() {
        if access.unreachable.is_empty() && access.refused.is_empty() {
            return Ok(None);
        }
        return Err(no_server_access(access, Vec::new(), None));
    }
    let held = held_servers(&access.connections);
    let connections = named(std::mem::take(&mut access.connections), &access.names);
    match dial_connections(matches, connections.clone()).await {
        Ok(client) => Ok(Some((client, connections))),
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

/// Cloud's `connections`, each carrying its Server's name where Cloud holds one.
fn named(
    connections: Vec<crate::context::Connection>,
    names: &BTreeMap<ployz_core::MachineId, String>,
) -> Vec<crate::context::Connection> {
    connections
        .into_iter()
        .map(|connection| {
            match connection
                .machine_id()
                .and_then(|id| names.get(id))
                .cloned()
            {
                Some(name) => connection.with_name(name),
                None => connection,
            }
        })
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

pub(super) fn no_servers() -> Error {
    Error::conflict("this Organization has no Servers")
        .hint(Hint::Next("ployz server add".into()))
        .with_connect_failure(ConnectFailure::NoServers)
}

/// Whether `error` is [`no_servers`]: Cloud knows no Server to look in.
pub(super) fn has_no_servers(error: &Error) -> bool {
    error.connect_failure() == Some(&ConnectFailure::NoServers)
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
    .with_connect_failure(ConnectFailure::Refused)
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
        // This CLI's own connection to it failed: Cloud reached it to hand that over.
        [one] if let Some(cause) = &cause => crate::connect::not_answering(one, relayed(cause)),
        [one] => format!(
            "Server {one} isn't answering. If it just joined or restarted, try again in a moment."
        ),
        _ => format!(
            "None of your Servers is answering: {}. If one just joined or restarted, try again in a moment.",
            super::joined(&shown)
        ),
    };
    let relayed = cause.as_ref().is_none_or(relayed);
    Error::detailed_caused(
        RpcErrorCode::Unavailable,
        message,
        json!({ "unreachable": unreachable, "servers": shown }),
        cause,
    )
    .with_connect_failure(ConnectFailure::Unanswered {
        servers: shown,
        relayed,
    })
}

/// Whether `error` is a connection whose last try went through Ployz Cloud's relay.
fn relayed(error: &ConnectError) -> bool {
    matches!(error, ConnectError::AllFailed { relayed: true, .. })
}

/// The Servers of a Cluster none of whose Servers answered, by name where the failure
/// says which, empty when it doesn't, and whether it asked through Ployz Cloud's relay.
pub(super) struct Silent {
    names: Vec<String>,
    relayed: bool,
}

/// Whether `error` is a Server of Cloud's refusing to give this command access.
pub(super) fn refused(error: &Error) -> bool {
    error.connect_failure() == Some(&ConnectFailure::Refused)
}

/// Whether `error` is Servers Cloud knows that didn't answer, and none that refused.
pub(super) fn servers_unanswered(error: &Error) -> bool {
    matches!(
        error.connect_failure(),
        Some(ConnectFailure::Unanswered { .. })
    )
}

impl Silent {
    /// Whether `error` is a connection no Server answered: Servers Cloud knows that
    /// didn't, or a dial whose every connection went unanswered. `None` when a daemon
    /// answered, or the failure is about something else, such as Cloud itself.
    pub(super) fn of(error: &Error) -> Option<Self> {
        if let Some(ConnectFailure::Unanswered { servers, relayed }) = error.connect_failure() {
            return Some(Self {
                names: servers.clone(),
                relayed: *relayed,
            });
        }
        let ConnectError::AllFailed {
            reached: false,
            attempts,
            server,
            relayed,
            ..
        } = error.connect_error()?
        else {
            return None;
        };
        // With more than one connection, the one Server named is only the last tried.
        Some(Self {
            names: if *attempts == 1 {
                server.iter().cloned().collect()
            } else {
                Vec::new()
            },
            relayed: *relayed,
        })
    }

    pub(super) fn names(&self) -> &[String] {
        &self.names
    }

    /// What to check, for the Server or Servers named.
    pub(super) fn next(&self) -> &'static str {
        match (self.names.as_slice(), self.relayed) {
            ([_], false) => "Check that its Ployz daemon is running.",
            (_, false) => "Check that the Ployz daemon is running on your Servers.",
            ([_], true) => {
                "Check that it is online, its Ployz daemon is running, and its outgoing https \
                 isn't filtered."
            }
            (_, true) => {
                "Check that your Servers are online, their Ployz daemon is running, and their \
                 outgoing https isn't filtered."
            }
        }
    }
}

impl std::fmt::Display for Silent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let through = if self.relayed {
            " through Ployz Cloud"
        } else {
            ""
        };
        match self.names.as_slice() {
            [] => write!(formatter, "No Server is answering{through}"),
            [one] => write!(formatter, "Server {one} isn't answering{through}"),
            many => write!(
                formatter,
                "None of your Servers is answering{through} ({})",
                super::joined(many)
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::leaf_matches;

    fn config_in(dir: &std::path::Path) -> ArgMatches {
        let config = dir.join("config.json");
        crate::cli::command()
            .try_get_matches_from([
                "ployz",
                "--ployz-config",
                config.to_str().unwrap(),
                "server",
                "ls",
            ])
            .unwrap()
    }

    #[tokio::test]
    async fn a_command_enters_by_its_credential_unless_it_names_a_cluster() {
        let dir = tempfile::tempdir().unwrap();
        let matches = config_in(dir.path());
        let leaf = leaf_matches(&matches);
        assert!(matches!(
            access(leaf, None).await,
            Ok(Access::Direct { signed_out: true })
        ));

        std::fs::write(dir.path().join("cloud.json"), "not a sign-in").unwrap();
        assert!(
            access(leaf, None).await.is_err(),
            "a broken login is not signed out"
        );
        assert!(connect_full(leaf, None).await.is_err());
        assert!(connect_or_none(leaf, None).await.is_err());
        assert!(entries(leaf, None).await.is_err());
        let Err(unreached) = reach_or_silent(leaf, None).await else {
            panic!("a broken login reached a Server");
        };
        assert!(unreached.silent.is_none());
        assert!(matches!(
            access(leaf, Some("prod")).await,
            Ok(Access::Direct { signed_out: false })
        ));
    }

    #[test]
    fn why_no_server_carried_a_command_is_a_value_not_a_sentence() {
        let none = no_servers();
        assert!(has_no_servers(&none));
        assert!(!servers_unanswered(&none) && !refused(&none));
        assert_eq!(none.report().message, "this Organization has no Servers");
        assert!(!has_no_servers(&Error::conflict(
            "this Organization has no Servers"
        )));
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
    async fn silent_servers_are_named_as_cloud_names_them() {
        let matches = crate::cli::command()
            .try_get_matches_from(["ployz", "status"])
            .unwrap();
        let (held, gone) = ("a".repeat(32), "b".repeat(32));
        let access = |unreachable: Vec<&String>| -> cloud_account::ServerAccess {
            serde_json::from_value(json!({
                "connections": [
                    { "machine_id": held, "unix": "/nonexistent/ployz-test/daemon.sock" }
                ],
                "unreachable": unreachable,
                "names": { held.clone(): "web-1" },
            }))
            .unwrap()
        };
        let silent = |access| async {
            let Err(error) = dial(leaf_matches(&matches), access).await else {
                panic!("dialed a Server that is not there");
            };
            Silent::of(&error).expect("no Server answered")
        };

        let one = silent(access(Vec::new())).await;
        assert_eq!(one.to_string(), "Server web-1 isn't answering");
        assert_eq!(one.next(), "Check that its Ployz daemon is running.");
        let both = silent(access(vec![&gone])).await;
        assert_eq!(
            both.to_string(),
            format!("None of your Servers is answering (web-1, {gone})")
        );
        assert_eq!(both.names(), ["web-1".to_owned(), gone]);
    }

    #[test]
    fn a_server_silent_through_cloud_may_be_offline_or_filtered() {
        let error = Error::from(ConnectError::AllFailed {
            selection: crate::context::ConnectionSource::Cloud,
            attempts: 1,
            server: Some("machine-1".into()),
            relayed: true,
            retry: ployz_core::Retry::AfterRestart,
            reached: false,
            last: None,
        });
        let silent = Silent::of(&error).expect("no Server answered");
        assert_eq!(
            format!("{silent}. {}", silent.next()),
            "Server machine-1 isn't answering through Ployz Cloud. Check that it is online, its \
             Ployz daemon is running, and its outgoing https isn't filtered."
        );
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
        assert!(!servers_unanswered(&error), "{report:?}");
        let Err(error) = answering(matches, access(&[])).await else {
            panic!("a Server that refused is not unanswered");
        };
        assert!(!servers_unanswered(&error));

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
        assert!(!servers_unanswered(&error), "{report:?}");
        assert!(Silent::of(&error).is_none(), "{report:?}");
    }

    #[test]
    fn full_scope_dials_management_connections_as_the_device() {
        use std::os::unix::fs::PermissionsExt;

        use crate::context::Transport;
        use ployz_core::{ManagementCapability, ManagementIdentity};

        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("device.key");
        let operator = ManagementCapability::new(ManagementIdentity::from_bytes([1; 32]), [2; 32]);
        let ssh: Connection = "root@node-a".parse().unwrap();
        let context = vec![
            Connection::management(operator.to_secret_string()).unwrap(),
            ssh.clone(),
        ];
        let dialed = |connections| {
            let dialed = as_device(connections, &key).unwrap();
            let [management, other] = dialed.as_slice() else {
                panic!("the context's two connections");
            };
            let Transport::Management(capability) = management.transport() else {
                panic!("the context's first connection is a Management Capability");
            };
            assert_eq!(other, &ssh);
            capability.to_secret_string()
        };

        assert_eq!(dialed(context.clone()), operator.to_secret_string());
        std::fs::write(&key, [3; 32]).unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            dialed(context.clone()),
            ManagementCapability::new(*operator.machine(), [3; 32]).to_secret_string()
        );

        std::fs::write(&key, b"broken").unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(as_device(vec![ssh.clone()], &key).unwrap(), vec![ssh]);
        assert!(as_device(context, &key).is_err());
    }
}
