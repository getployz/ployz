use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use ployz_core::{
    ContainerId, ContainerKind, ContainerLocator, ContainerObservation,
    ContainerRuntimeObservation, ContainerWant, Drain, EnvironmentValues, HealthObservation,
    MachineId, MachineObservation, MachineStorageObservation, Namespace, RpcError, RpcErrorCode,
    RunTarget, ServiceName, StopOptions,
};
use ployz_store::DeploymentId;
use ployz_store::tools::cloud::{
    ContainerReport, ContainerState, ContainersAnswer, Disk, Health, ServerReport, ServerState,
    ServersAnswer,
};
use ployz_store::tools::{CloudQuery, Freshness, ServerName, ServerRef, Source, Timestamp};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::Session;
use crate::cluster::HealthWaitError;
use crate::connect::TARGET_RPC_TIMEOUT;
use crate::operator::DEPLOYMENT_LABEL;

/// Answer `query` from the Servers behind `session`. A Server that did not answer is
/// named in `seen.unanswered`, never guessed.
///
/// # Errors
/// Returns `unsupported` for a query only Cloud answers, and
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
            let mut unanswered = uninvited(&machines);
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
            let mut unanswered = uninvited(&machines);
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

/// One Container of a Service as its Server reports it: what an act stops and starts.
/// A hook Container is stopped with its Service but never started.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ServiceCopy {
    pub server: ServerName,
    pub machine: MachineId,
    pub container: ContainerId,
    pub state: ContainerState,
    pub hook: bool,
}

/// Every Container of one Service, and the Servers that did not say, by ID so a rename can't hide one.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ServiceCopies {
    pub copies: Vec<ServiceCopy>,
    pub unanswered: Vec<ServerRef>,
}

/// The Containers of `service` in `namespace` on every Server that answers.
///
/// # Errors
/// Returns the session's error when it is closed or the Servers cannot be listed.
pub async fn observe_service(
    session: &Session,
    namespace: &Namespace,
    service: &ServiceName,
) -> Result<ServiceCopies, RpcError> {
    let mut client = session.client()?;
    session
        .until_closed(async {
            let machines = client.machines().await.map_err(RpcError::from)?;
            let live = client
                .live_services_from(&machines, EnvironmentValues::Redacted)
                .await
                .map_err(RpcError::from)?;
            let mut unanswered = uninvited(&machines);
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
            let copies = copies_of(
                live.containers
                    .successes
                    .iter()
                    .flat_map(|success| &success.value),
                &names,
                namespace,
                service,
            );
            Ok(ServiceCopies {
                copies,
                unanswered: machines
                    .iter()
                    .filter(|machine| unanswered.contains(&machine.machine.id))
                    .map(|machine| ServerRef {
                        machine: machine.machine.id,
                        name: machine.machine.name.clone(),
                    })
                    .collect(),
            })
        })
        .await
}

fn copies_of<'a>(
    containers: impl Iterator<Item = &'a ContainerObservation>,
    names: &BTreeMap<MachineId, &ServerName>,
    namespace: &Namespace,
    service: &ServiceName,
) -> Vec<ServiceCopy> {
    containers
        .filter(|container| {
            &container.namespace == namespace && container.service_name() == service
        })
        .filter_map(|container| {
            let server = names.get(&container.machine_id)?;
            Some(ServiceCopy {
                server: (*server).clone(),
                machine: container.machine_id,
                container: container.container_id,
                state: report(container, server).state,
                hook: container.kind == ContainerKind::PreDeployHook,
            })
        })
        .collect()
}

/// Stop Container `container` on `machine`, after its Servers' proxies stop routing to
/// it or the default drain ends. An answer is the Machine's; a lost one is the
/// transport's error, and the stop may have happened.
///
/// # Errors
/// Returns the Machine's refusal, or the transport's error when it did not answer.
pub async fn stop_one(
    session: &Session,
    machine: MachineId,
    container: ContainerId,
) -> Result<(), RpcError> {
    let client = session.client()?;
    session
        .until_closed(client.ensure_container(
            &machine,
            ContainerWant::Stopped {
                container: ContainerLocator::Id(container),
                drain: Drain::DEFAULT,
                stop: StopOptions {
                    signal: None,
                    grace_period_seconds: None,
                },
            },
            None,
        ))
        .await
        .map(|_| ())
}

/// Remove Container `container` from `machine` with its anonymous Volumes, keeping its
/// named Volumes. A Container the Machine no longer has answers Ok, so a resend after a
/// crash converges.
///
/// # Errors
/// Returns the Machine's refusal, or the transport's error when it did not answer.
pub async fn remove_one(
    session: &Session,
    machine: MachineId,
    container: ContainerId,
) -> Result<(), RpcError> {
    let client = session.client()?;
    session
        .until_closed(client.ensure_container(
            &machine,
            ContainerWant::Gone {
                container: ContainerLocator::Id(container),
                drain: Drain::DEFAULT,
            },
            None,
        ))
        .await
        .map(|_| ())
}

/// What a committed start reached, waiting for it the way a Deploy does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Started {
    /// The Container started and passed its healthcheck, or has none.
    Serving,
    /// The Container started but its health could not be read.
    Unconfirmed,
    /// The Container started but failed its healthcheck, or did not pass it in time.
    Unhealthy,
}

impl Started {
    /// The wire name the dashboard reads.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Serving => "serving",
            Self::Unconfirmed => "unconfirmed",
            Self::Unhealthy => "unhealthy",
        }
    }
}

/// Start Container `container` on `machine`, then wait until it is healthy or past its
/// healthcheck deadline.
///
/// # Errors
/// Returns the Machine's refusal, or the transport's error when it did not answer.
pub async fn start_one(
    session: &Session,
    machine: MachineId,
    container: ContainerId,
) -> Result<Started, RpcError> {
    let client = session.client()?;
    session
        .until_closed(async {
            client
                .ensure_container(
                    &machine,
                    ContainerWant::Running {
                        container: RunTarget::Existing(ContainerLocator::Id(container)),
                    },
                    Some(TARGET_RPC_TIMEOUT),
                )
                .await?;
            let cancellation = CancellationToken::new();
            Ok(
                match client
                    .wait_until_ready(&machine, &container, &cancellation)
                    .await
                {
                    Ok(()) => Started::Serving,
                    Err(HealthWaitError::Health(_)) => Started::Unhealthy,
                    Err(HealthWaitError::Inspect(_)) => Started::Unconfirmed,
                },
            )
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

fn uninvited(machines: &[MachineObservation]) -> BTreeSet<MachineId> {
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

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
