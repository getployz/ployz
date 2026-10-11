//! What Ployz Cloud's Machines answer for the agent tools: the [`CloudQuery`]s a
//! Session can answer alone. Copies need the Store to name each Volume and Logs need
//! its secrets, so Cloud's adapter answers those, as it does Operations and GitHub.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use ployz_core::{
    ContainerKind, ContainerObservation, ContainerRuntimeObservation, EnvironmentValues,
    HealthObservation, MachineId, MachineObservation, MachineStorageObservation, Namespace,
    RpcError, RpcErrorCode, ServiceName,
};
use ployz_store::DeploymentId;
use ployz_store::tools::cloud::{
    ContainerReport, ContainerState, ContainersAnswer, Disk, Health, ServerReport, ServerState,
    ServersAnswer,
};
use ployz_store::tools::{CloudQuery, Freshness, ServerName, Source, Timestamp};
use serde_json::Value;

use super::Session;

/// The label ployzd puts on each container a Deployment creates.
const DEPLOYMENT_LABEL: &str = "ployz.deployment.id";

/// Answer `query` from the Servers behind `session`. A Server that did not answer is
/// named in `seen.unanswered`, never guessed.
///
/// # Errors
/// Returns `unsupported` for a query Cloud answers without the Machines alone, and
/// the session's error when it is closed or the Servers cannot be listed.
pub async fn ask(session: &Session, query: CloudQuery) -> Result<Value, RpcError> {
    match query {
        CloudQuery::Servers { only } => encode(&servers(session, only.as_ref()).await?),
        CloudQuery::Containers { namespace, service } => {
            encode(&containers(session, &namespace, service.as_ref()).await?)
        }
        CloudQuery::Copies { .. } | CloudQuery::Logs(_) => Err(unsupported(
            "Copies and logs join the Config Store; Ployz Cloud answers them",
        )),
        CloudQuery::Operations(_)
        | CloudQuery::GithubTree(_)
        | CloudQuery::GithubFile(_)
        | CloudQuery::GithubConnection
        | CloudQuery::GithubBranches { .. } => Err(unsupported(
            "Operations and GitHub are Ployz Cloud's records; the Servers do not hold them",
        )),
    }
}

/// Every Server, or the one named `only`, with its storage and Namespaces.
async fn servers(session: &Session, only: Option<&ServerName>) -> Result<ServersAnswer, RpcError> {
    let mut client = session.client()?;
    session
        .until_closed(async {
            let mut machines = client.machines().await.map_err(RpcError::from)?;
            if let Some(only) = only {
                machines.retain(|machine| &machine.machine.name == only);
            }
            let gaps = client.observe_machine_storage(&mut machines).await;
            let live = client
                .live_services_from(&machines, EnvironmentValues::Redacted)
                .await
                .map_err(RpcError::from)?;
            let mut unanswered = silent(&machines);
            unanswered.extend(gaps.failures.iter().map(|failure| failure.machine_id));
            unanswered.extend(gaps.omissions.iter().copied());
            unanswered.extend(
                live.containers
                    .failures
                    .iter()
                    .map(|failure| failure.machine_id),
            );
            unanswered.extend(live.containers.omissions.iter().copied());
            let mut namespaces = BTreeMap::<MachineId, BTreeSet<Namespace>>::new();
            for container in live.containers.successes.iter().flat_map(|s| &s.value) {
                namespaces
                    .entry(container.machine_id)
                    .or_default()
                    .insert(container.namespace.clone());
            }
            let servers = machines
                .iter()
                .map(|machine| server(machine, &unanswered, &mut namespaces))
                .collect();
            Ok(ServersAnswer {
                servers,
                seen: seen(&machines, &unanswered),
            })
        })
        .await
}

fn server(
    observed: &MachineObservation,
    unanswered: &BTreeSet<MachineId>,
    namespaces: &mut BTreeMap<MachineId, BTreeSet<Namespace>>,
) -> ServerReport {
    let machine = &observed.machine;
    let release = &machine.runtime.daemon_version;
    ServerReport {
        machine: machine.id,
        name: machine.name.clone(),
        state: if unanswered.contains(&machine.id) {
            ServerState::Offline
        } else {
            ServerState::Online
        },
        release: (!release.is_empty()).then(|| release.clone()),
        // The newest release on a channel is Cloud's to know, not the Server's.
        upgrade: None,
        draining: !machine.accepts_services,
        disk: match observed.storage {
            Some(MachineStorageObservation::Pool {
                size_bytes,
                used_bytes,
                ..
            }) => Some(Disk {
                used_bytes,
                total_bytes: size_bytes.get(),
            }),
            Some(MachineStorageObservation::Stateless | MachineStorageObservation::Ready)
            | None => None,
        },
        namespaces: namespaces
            .remove(&machine.id)
            .unwrap_or_default()
            .into_iter()
            .collect(),
    }
}

/// `namespace`'s Service containers, or `service`'s only, with redacted env.
async fn containers(
    session: &Session,
    namespace: &Namespace,
    service: Option<&ServiceName>,
) -> Result<ContainersAnswer, RpcError> {
    let mut client = session.client()?;
    session
        .until_closed(async {
            let machines = client.machines().await.map_err(RpcError::from)?;
            let live = client
                .live_services_from(&machines, EnvironmentValues::Redacted)
                .await
                .map_err(RpcError::from)?;
            let mut unanswered = silent(&machines);
            unanswered.extend(
                live.containers
                    .failures
                    .iter()
                    .map(|failure| failure.machine_id),
            );
            unanswered.extend(live.containers.omissions.iter().copied());
            let names: BTreeMap<MachineId, &ServerName> = machines
                .iter()
                .map(|machine| (machine.machine.id, &machine.machine.name))
                .collect();
            let containers = live
                .containers
                .successes
                .iter()
                .flat_map(|success| &success.value)
                .filter(|container| {
                    container.kind == ContainerKind::ServiceContainer
                        && &container.namespace == namespace
                        && service.is_none_or(|service| container.service_name() == service)
                })
                .filter_map(|container| {
                    let server = names.get(&container.machine_id)?;
                    Some(report(container, server))
                })
                .collect();
            Ok(ContainersAnswer {
                containers,
                servers: machines
                    .iter()
                    .map(|machine| machine.machine.name.clone())
                    .collect(),
                seen: seen(&machines, &unanswered),
            })
        })
        .await
}

fn report(container: &ContainerObservation, server: &ServerName) -> ContainerReport {
    let (state, exit_code, health) = match &container.runtime {
        ContainerRuntimeObservation::Created => (ContainerState::Created, None, None),
        ContainerRuntimeObservation::Running { health } => {
            (ContainerState::Running, None, self::health(health))
        }
        ContainerRuntimeObservation::Paused => (ContainerState::Paused, None, None),
        ContainerRuntimeObservation::Restarting => (ContainerState::Restarting, None, None),
        ContainerRuntimeObservation::Exited { code } => (ContainerState::Exited, Some(*code), None),
        ContainerRuntimeObservation::Removing => (ContainerState::Exited, None, None),
        // A state Docker reports that this release does not know is not running.
        ContainerRuntimeObservation::Dead | ContainerRuntimeObservation::Unknown { .. } => {
            (ContainerState::Dead, None, None)
        }
    };
    ContainerReport {
        server: server.clone(),
        service: container.service_name().clone(),
        state,
        exit_code,
        health,
        restarts: None,
        image: container.resolved_spec.container.image.clone(),
        deployment: container
            .labels
            .get(DEPLOYMENT_LABEL)
            .and_then(|id| DeploymentId::parse(id.clone()).ok()),
    }
}

fn health(observed: &HealthObservation) -> Option<Health> {
    match observed {
        HealthObservation::Starting => Some(Health::Starting),
        HealthObservation::Healthy => Some(Health::Healthy),
        HealthObservation::Unhealthy | HealthObservation::Failing => Some(Health::Unhealthy),
        HealthObservation::NotConfigured
        | HealthObservation::Stopping
        | HealthObservation::Unrecognized(_) => None,
    }
}

/// The Machines membership says not to ask.
fn silent(machines: &[MachineObservation]) -> BTreeSet<MachineId> {
    machines
        .iter()
        .filter(|machine| !machine.invites_rpc())
        .map(|machine| machine.machine.id)
        .collect()
}

fn seen(machines: &[MachineObservation], unanswered: &BTreeSet<MachineId>) -> Freshness {
    Freshness {
        seen_by: None,
        observed_at: None,
        read_at: now(),
        age_seconds: None,
        from: Source::Server,
        unanswered: machines
            .iter()
            .filter(|machine| unanswered.contains(&machine.machine.id))
            .map(|machine| machine.machine.name.clone())
            .collect(),
    }
}

fn now() -> Timestamp {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    Timestamp(i64::try_from(seconds).unwrap_or(i64::MAX))
}

fn encode(answer: &impl serde::Serialize) -> Result<Value, RpcError> {
    serde_json::to_value(answer).map_err(|error| RpcError {
        code: RpcErrorCode::Internal,
        message: format!("encoding a Cloud answer: {error}"),
        details: Value::Null,
        cause: Vec::new(),
    })
}

fn unsupported(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unsupported,
        message: message.to_owned(),
        details: Value::Null,
        cause: Vec::new(),
    }
}
