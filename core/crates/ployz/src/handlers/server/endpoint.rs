//! `server set --wg-endpoint`: every other Server the command line enters by dials the
//! target at the new endpoint, so a mesh that lost the target heals without it.

use std::sync::Arc;

use futures_util::future::join_all;
use ployz_core::{
    Machine, MachineId, MachineName, MachineTarget, RpcError, RpcErrorCode, SelectEndpointRequest,
    SelectedEndpoint, op,
};
use serde::Serialize;

use crate::{
    connect::{Client, Connector, TARGET_RPC_TIMEOUT, apply_timeout, connect_one, rpc_error},
    context::{Connection, ConnectionSource, SelectedConnections},
};

/// What one entry did with the operator's endpoint.
enum Entry {
    /// The entry is the target itself; it takes the record update instead.
    Target {
        client: Client,
        target: Machine,
    },
    Dialing {
        client: Client,
        target: Machine,
        observer: Dialer,
    },
    /// The entry answered but did not take the endpoint; it can still update the target.
    Refused {
        client: Client,
        target: Machine,
        missed: Missed,
    },
    Missed(Missed),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct Dialer {
    pub(super) id: MachineId,
    pub(super) name: MachineName,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(super) struct Missed {
    /// The Server's name once reached, else the connection.
    pub(super) entry: String,
    pub(super) error: RpcError,
}

impl Missed {
    pub(super) fn sentence(&self) -> String {
        if self.error.code == RpcErrorCode::Unavailable {
            format!("{} unreachable.", self.entry)
        } else if self.error.code == RpcErrorCode::Unsupported {
            format!("{} needs a daemon upgrade to take it.", self.entry)
        } else {
            format!("{} refused it: {}", self.entry, self.error.message)
        }
    }
}

/// The endpoint and the Servers that now dial the target at it.
#[derive(Serialize)]
pub(super) struct Seeding {
    pub(super) address: SelectedEndpoint,
    pub(super) dialed_by: Vec<Dialer>,
    pub(super) failures: Vec<Missed>,
    #[serde(skip)]
    pub(super) target: Option<Machine>,
    /// A client entering by the target itself, else by any reached Server.
    #[serde(skip)]
    pub(super) updater: Option<Client>,
}

impl Seeding {
    /// `m2, m3 now dial m1 at …`, or `None` when no Server took it.
    pub(super) fn sentence(&self) -> Option<String> {
        let target = self.target.as_ref()?;
        let (first, rest) = self.dialed_by.split_first()?;
        let verb = if rest.is_empty() { "dials" } else { "dial" };
        let names = std::iter::once(first)
            .chain(rest)
            .map(|dialer| dialer.name.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!(
            "{names} now {verb} {} at {}.",
            target.name, self.address.0
        ))
    }
}

/// Hand `address` for `target` to every Server `selected` enters by, other than `target`.
pub(super) async fn seed(
    selected: &SelectedConnections,
    connector: Arc<dyn Connector>,
    target: &MachineTarget,
    address: SelectedEndpoint,
) -> Seeding {
    let entries = join_all(
        selected
            .connections
            .iter()
            .map(|connection| enter(connection, &selected.source, &connector, target, address)),
    )
    .await;
    let mut seeding = Seeding {
        address,
        dialed_by: Vec::new(),
        failures: Vec::new(),
        target: None,
        updater: None,
    };
    let mut fallback = None;
    for entry in entries {
        match entry {
            Entry::Target { client, target } => {
                seeding.target.get_or_insert(target);
                seeding.updater.get_or_insert(client);
            }
            Entry::Dialing {
                client,
                target,
                observer,
            } => {
                seeding.target.get_or_insert(target);
                fallback.get_or_insert(client);
                if !seeding.dialed_by.contains(&observer) {
                    seeding.dialed_by.push(observer);
                }
            }
            Entry::Refused {
                client,
                target,
                missed,
            } => {
                seeding.target.get_or_insert(target);
                fallback.get_or_insert(client);
                seeding.failures.push(missed);
            }
            Entry::Missed(missed) => seeding.failures.push(missed),
        }
    }
    if seeding.updater.is_none() {
        seeding.updater = fallback;
    }
    seeding
}

async fn enter(
    connection: &Connection,
    source: &ConnectionSource,
    connector: &Arc<dyn Connector>,
    target: &MachineTarget,
    address: SelectedEndpoint,
) -> Entry {
    let missed = |entry: String, error| Entry::Missed(Missed { entry, error });
    let mut client = match connect_one(connection, source, connector, false).await {
        Ok(client) => client,
        Err(error) => {
            let unreachable = error.is_unreachable();
            let mut error = rpc_error(error);
            if unreachable {
                error.code = RpcErrorCode::Unavailable;
            }
            return missed(connection.to_string(), error);
        }
    };
    let observed = match (client.entry_id().await, client.machines().await) {
        (Ok(id), Ok(machines)) => machines
            .iter()
            .find(|observation| observation.machine.id == id)
            .map(|observation| observation.machine.name.clone())
            .map(|name| (Dialer { id, name }, machines)),
        (Err(error), _) | (_, Err(error)) => {
            return missed(connection.to_string(), rpc_error(error));
        }
    };
    let Some((observer, machines)) = observed else {
        return missed(
            connection.to_string(),
            rpc_error(crate::connect::ConnectError::MissingMachineDetails),
        );
    };
    let target = match crate::cluster::visible_machine(target, &machines) {
        Ok(found) => found.machine.clone(),
        Err(error) => return missed(observer.name.to_string(), error),
    };
    if target.id == observer.id {
        return Entry::Target { client, target };
    }
    let request = SelectEndpointRequest {
        peer: target.id,
        endpoint: address,
    };
    match apply_timeout(
        Some(TARGET_RPC_TIMEOUT),
        client.call::<op::SelectEndpoint>(request, None),
    )
    .await
    {
        Ok(_) => Entry::Dialing {
            client,
            target,
            observer,
        },
        Err(error) => Entry::Refused {
            client,
            target,
            missed: Missed {
                entry: observer.name.to_string(),
                error: rpc_error(error),
            },
        },
    }
}
