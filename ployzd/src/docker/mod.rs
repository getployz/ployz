mod spec_store;

#[cfg(test)]
mod integration_tests;

use std::{collections::HashMap, net::Ipv4Addr, time::Duration, time::SystemTime};

use bollard::{
    Docker,
    models::ContainerInspectResponse,
    query_parameters::{EventsOptionsBuilder, ListContainersOptionsBuilder},
};
use futures_util::StreamExt;
use ployz_core::{
    ContainerAddress, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, HealthObservation, MachineId, ServiceId, ServiceName, ValueError,
};
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;
use tokio::sync::watch;

use crate::corrosion::{LocalContainerSnapshot, ReplicatedStore};

pub use spec_store::{Error as SpecStoreError, MachineSpecStore};

pub const LABEL_MANAGED: &str = "ployz.managed";
pub const LABEL_SERVICE_ID: &str = "ployz.service.id";
pub const LABEL_SERVICE_NAME: &str = "ployz.service.name";
pub const LABEL_HOOK: &str = "ployz.service.hook";
pub const LABEL_HOOK_PRE_DEPLOY: &str = "pre-deploy";
pub const RESCAN_INTERVAL: Duration = Duration::from_secs(30);
const EVENT_DEBOUNCE: Duration = Duration::from_millis(100);

#[derive(Clone)]
pub struct LocalDocker {
    client: Docker,
}

impl LocalDocker {
    pub fn connect() -> Result<Self, Error> {
        Ok(Self {
            client: Docker::connect_with_defaults()?,
        })
    }

    #[cfg(test)]
    fn connect_socket(socket: &str) -> Result<Self, Error> {
        let socket = socket.trim_start_matches("unix://");
        Ok(Self {
            client: Docker::connect_with_socket(socket, 120, bollard::API_DEFAULT_VERSION)?,
        })
    }

    async fn managed_container_ids(&self) -> Result<Vec<ContainerId>, Error> {
        let filters = HashMap::from([("label", vec![LABEL_MANAGED, LABEL_SERVICE_ID])]);
        let options = ListContainersOptionsBuilder::default()
            .all(true)
            .filters(&filters)
            .build();
        self.client
            .list_containers(Some(options))
            .await?
            .into_iter()
            .map(|container| {
                let id = container.id.ok_or(Error::MissingField("container ID"))?;
                ContainerId::parse(id).map_err(|source| Error::InvalidValue {
                    field: "container ID",
                    source,
                })
            })
            .collect()
    }

    async fn inspect_managed(
        &self,
        container_id: &ContainerId,
        machine_id: &MachineId,
        specs: &MachineSpecStore,
    ) -> Result<ContainerObservation, Error> {
        let inspected = match self
            .client
            .inspect_container(container_id.as_str(), None)
            .await
        {
            Ok(inspected) => RawContainerInspect::from_typed(&inspected)?,
            Err(bollard::errors::Error::JsonDataError { contents, .. }) => {
                serde_json::from_str(&contents)?
            }
            Err(error) => return Err(error.into()),
        };
        let address = container_address(&inspected);
        let labels = inspected
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .ok_or(Error::MissingField("container labels"))?;
        let managed = ManagedLabels::parse(labels)?;
        let resolved_spec = specs
            .get(container_id)
            .await?
            .ok_or_else(|| Error::SpecNotFound(container_id.clone()))?;

        Ok(ContainerObservation {
            container_id: container_id.clone(),
            display_name: display_name(inspected.name.as_deref()),
            machine_id: machine_id.clone(),
            service_id: managed.service_id,
            service_name: managed.service_name,
            kind: managed.kind,
            runtime: runtime_observation(inspected.state.as_ref()),
            resolved_spec,
            address,
            labels: labels
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        })
    }
}

fn display_name(name: Option<&str>) -> String {
    name.unwrap_or_default().trim_start_matches('/').to_owned()
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawContainerInspect {
    name: Option<String>,
    config: Option<RawContainerConfig>,
    state: Option<serde_json::Value>,
    network_settings: Option<RawNetworkSettings>,
}

impl RawContainerInspect {
    fn from_typed(inspected: &ContainerInspectResponse) -> Result<Self, serde_json::Error> {
        Ok(Self {
            name: inspected.name.clone(),
            config: inspected.config.as_ref().map(|config| RawContainerConfig {
                labels: config.labels.clone(),
            }),
            state: inspected
                .state
                .as_ref()
                .map(serde_json::to_value)
                .transpose()?,
            network_settings: Some(RawNetworkSettings {
                networks: inspected
                    .network_settings
                    .as_ref()
                    .and_then(|settings| settings.networks.as_ref())
                    .map(|networks| {
                        networks
                            .iter()
                            .map(|(name, endpoint)| {
                                (
                                    name.clone(),
                                    RawEndpointSettings {
                                        ip_address: endpoint.ip_address.clone(),
                                    },
                                )
                            })
                            .collect()
                    }),
            }),
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawContainerConfig {
    labels: Option<HashMap<String, String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawNetworkSettings {
    networks: Option<HashMap<String, RawEndpointSettings>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawEndpointSettings {
    ip_address: Option<String>,
}

pub struct ContainerObserver {
    docker: LocalDocker,
    specs: MachineSpecStore,
    replicated: ReplicatedStore,
    machine_id: MachineId,
    rescan_interval: Duration,
}

impl ContainerObserver {
    #[must_use]
    pub fn new(
        docker: LocalDocker,
        specs: MachineSpecStore,
        replicated: ReplicatedStore,
        machine_id: MachineId,
    ) -> Self {
        Self {
            docker,
            specs,
            replicated,
            machine_id,
            rescan_interval: RESCAN_INTERVAL,
        }
    }

    #[cfg(test)]
    fn with_rescan_interval(mut self, interval: Duration) -> Self {
        self.rescan_interval = interval;
        self
    }

    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) -> Result<(), Error> {
        while !*shutdown.borrow() {
            if let Err(error) = self.watch(&mut shutdown).await {
                eprintln!("local Docker observation failed, retrying: {error}");
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(1)) => {}
                    changed = shutdown.changed() => {
                        changed.map_err(|_| Error::ShutdownClosed)?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn watch(&self, shutdown: &mut watch::Receiver<bool>) -> Result<(), Error> {
        let filters = HashMap::from([
            ("type", vec!["container"]),
            ("scope", vec!["local"]),
            ("label", vec![LABEL_MANAGED]),
            (
                "event",
                vec![
                    "create",
                    "start",
                    "stop",
                    "pause",
                    "unpause",
                    "kill",
                    "die",
                    "oom",
                    "destroy",
                    "health_status",
                ],
            ),
        ]);
        let since = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|error| Error::Clock(error.to_string()))?
            .as_secs()
            .to_string();
        let options = EventsOptionsBuilder::default()
            .since(&since)
            .filters(&filters)
            .build();
        // Bollard opens this lazy stream when first polled. The cursor replays any event
        // between capturing `since` and completing the initial snapshot.
        let mut events = Box::pin(self.docker.client.events(Some(options)));
        self.sync_once().await?;

        let mut rescans = tokio::time::interval(self.rescan_interval);
        rescans.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        rescans.tick().await;
        let mut scan_at = None;
        loop {
            tokio::select! {
                event = events.next() => match event {
                    Some(Ok(_)) => {
                        scan_at = Some(tokio::time::Instant::now() + EVENT_DEBOUNCE);
                    }
                    Some(Err(error)) => return Err(error.into()),
                    None => return Err(Error::EventStreamClosed),
                },
                _ = rescans.tick() => self.sync_once().await?,
                () = async {
                    match scan_at {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                } => {
                    scan_at = None;
                    self.sync_once().await?;
                }
                changed = shutdown.changed() => {
                    changed.map_err(|_| Error::ShutdownClosed)?;
                    return Ok(());
                }
            }
        }
    }

    async fn sync_once(&self) -> Result<(), Error> {
        // TODO(UT-120): preserve stale rows when Docker cannot provide a complete inventory.
        let inventory = self.docker.managed_container_ids().await?;
        let mut snapshot = LocalContainerSnapshot::from_inventory(inventory);
        let container_ids = snapshot.ids().cloned().collect::<Vec<_>>();
        for container_id in &container_ids {
            match self
                .docker
                .inspect_managed(container_id, &self.machine_id, &self.specs)
                .await
            {
                Ok(observation) => {
                    snapshot.observed(observation);
                }
                Err(error) => {
                    eprintln!("failed to inspect managed container {container_id}: {error}")
                }
            }
        }
        self.replicated
            .reconcile_local_containers(&self.machine_id, &snapshot)
            .await
            .map_err(Error::from)
    }
}

struct ManagedLabels {
    service_id: ServiceId,
    service_name: ServiceName,
    kind: ContainerKind,
}

impl ManagedLabels {
    fn parse(labels: &HashMap<String, String>) -> Result<Self, Error> {
        if !labels.contains_key(LABEL_MANAGED) {
            return Err(Error::NotManaged);
        }
        let service_id = required_label(labels, LABEL_SERVICE_ID)?;
        let service_name = required_label(labels, LABEL_SERVICE_NAME)?;
        let kind = match labels.get(LABEL_HOOK) {
            None => ContainerKind::ServiceContainer,
            Some(_) => ContainerKind::PreDeployHook,
        };
        Ok(Self {
            service_id: ServiceId::parse(service_id).map_err(|source| Error::InvalidValue {
                field: LABEL_SERVICE_ID,
                source,
            })?,
            service_name: ServiceName::parse(service_name).map_err(|source| {
                Error::InvalidValue {
                    field: LABEL_SERVICE_NAME,
                    source,
                }
            })?,
            kind,
        })
    }
}

fn required_label<'a>(
    labels: &'a HashMap<String, String>,
    name: &'static str,
) -> Result<&'a str, Error> {
    labels
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(Error::MissingLabel(name))
}

fn runtime_observation(state: Option<&serde_json::Value>) -> ContainerRuntimeObservation {
    let Some(state) = state else {
        return ContainerRuntimeObservation::Unknown {
            raw: json!({ "state": null }),
        };
    };
    let status = state
        .get("Status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let health = state
        .get("Health")
        .and_then(|health| health.get("Status"))
        .and_then(serde_json::Value::as_str);
    let exit_code = state.get("ExitCode").and_then(serde_json::Value::as_i64);
    runtime_from_parts_with_raw(status, exit_code, health, state)
}

#[cfg(test)]
fn runtime_from_parts(
    status: &str,
    exit_code: Option<i64>,
    health: Option<&str>,
) -> ContainerRuntimeObservation {
    runtime_from_parts_with_raw(
        status,
        exit_code,
        health,
        &json!({ "Status": status, "ExitCode": exit_code, "Health": health }),
    )
}

fn runtime_from_parts_with_raw(
    status: &str,
    exit_code: Option<i64>,
    health: Option<&str>,
    raw: &serde_json::Value,
) -> ContainerRuntimeObservation {
    match status {
        "created" => ContainerRuntimeObservation::Created,
        "running" => ContainerRuntimeObservation::Running {
            health: match health {
                None | Some("none") => HealthObservation::NotConfigured,
                Some("starting") => HealthObservation::Starting,
                Some("healthy") => HealthObservation::Healthy,
                Some("unhealthy") => HealthObservation::Unhealthy,
                Some(value) => HealthObservation::Unrecognized(value.to_owned()),
            },
        },
        "paused" => ContainerRuntimeObservation::Paused,
        "restarting" => ContainerRuntimeObservation::Restarting,
        "exited" if exit_code.is_some() => ContainerRuntimeObservation::Exited {
            code: exit_code.expect("checked"),
        },
        "removing" => ContainerRuntimeObservation::Removing,
        "dead" => ContainerRuntimeObservation::Dead,
        _ => ContainerRuntimeObservation::Unknown { raw: raw.clone() },
    }
}

fn container_address(inspected: &RawContainerInspect) -> Option<ContainerAddress> {
    inspected
        .network_settings
        .as_ref()?
        .networks
        .as_ref()?
        .get(crate::network::DOCKER_NETWORK_NAME)?
        .ip_address
        .as_deref()?
        .parse::<Ipv4Addr>()
        .ok()
        .map(ContainerAddress)
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Docker operation failed: {0}")]
    Docker(#[from] bollard::errors::Error),
    #[error("Docker inspect JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    SpecStore(#[from] SpecStoreError),
    #[error(transparent)]
    ReplicatedStore(#[from] crate::corrosion::Error),
    #[error("managed container is missing {0}")]
    MissingField(&'static str),
    #[error("managed container is missing label {0}")]
    MissingLabel(&'static str),
    #[error("managed container has invalid {field}: {source}")]
    InvalidValue {
        field: &'static str,
        #[source]
        source: ValueError,
    },
    #[error("container is not managed by Ployz")]
    NotManaged,
    #[error("resolved spec not found in machine.db for {0}")]
    SpecNotFound(ContainerId),
    #[error("Docker event stream closed")]
    EventStreamClosed,
    #[error("observer shutdown channel closed")]
    ShutdownClosed,
    #[error("system clock cannot be represented for Docker event replay: {0}")]
    Clock(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_labels_distinguish_service_and_hook_containers() {
        assert!(matches!(
            ManagedLabels::parse(&HashMap::new()),
            Err(Error::NotManaged)
        ));
        let mut labels = HashMap::from([
            (LABEL_MANAGED.to_owned(), String::new()),
            (LABEL_SERVICE_ID.to_owned(), "a".repeat(32)),
            (LABEL_SERVICE_NAME.to_owned(), "api".to_owned()),
        ]);
        assert_eq!(
            ManagedLabels::parse(&labels).unwrap().kind,
            ContainerKind::ServiceContainer
        );
        labels.insert(LABEL_HOOK.to_owned(), LABEL_HOOK_PRE_DEPLOY.to_owned());
        assert_eq!(
            ManagedLabels::parse(&labels).unwrap().kind,
            ContainerKind::PreDeployHook
        );
        labels.insert(LABEL_HOOK.to_owned(), "future-hook".to_owned());
        assert_eq!(
            ManagedLabels::parse(&labels).unwrap().kind,
            ContainerKind::PreDeployHook
        );
    }

    #[test]
    fn runtime_and_health_keep_unknown_values_explicit() {
        assert_eq!(
            runtime_from_parts("created", None, None),
            ContainerRuntimeObservation::Created
        );
        assert_eq!(
            runtime_from_parts("running", None, None),
            ContainerRuntimeObservation::Running {
                health: HealthObservation::NotConfigured
            }
        );
        assert_eq!(
            runtime_from_parts("running", None, Some("degraded")),
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Unrecognized("degraded".into())
            }
        );
        for (health, expected) in [
            ("starting", HealthObservation::Starting),
            ("healthy", HealthObservation::Healthy),
            ("unhealthy", HealthObservation::Unhealthy),
        ] {
            assert_eq!(
                runtime_from_parts("running", None, Some(health)),
                ContainerRuntimeObservation::Running { health: expected }
            );
        }
        assert_eq!(
            runtime_from_parts("paused", None, Some("healthy")),
            ContainerRuntimeObservation::Paused
        );
        assert_eq!(
            runtime_from_parts("restarting", None, Some("healthy")),
            ContainerRuntimeObservation::Restarting
        );
        assert_eq!(
            runtime_from_parts("exited", Some(17), None),
            ContainerRuntimeObservation::Exited { code: 17 }
        );
        assert_eq!(
            runtime_from_parts("removing", None, None),
            ContainerRuntimeObservation::Removing
        );
        assert_eq!(
            runtime_from_parts("dead", None, None),
            ContainerRuntimeObservation::Dead
        );
        assert!(matches!(
            runtime_from_parts("stopping", None, None),
            ContainerRuntimeObservation::Unknown { raw }
                if raw.get("Status") == Some(&json!("stopping"))
        ));
        assert!(matches!(
            runtime_from_parts("future", None, Some("future-health")),
            ContainerRuntimeObservation::Unknown { raw }
                if raw.get("Health") == Some(&json!("future-health"))
        ));
    }

    #[test]
    fn raw_docker_inspect_keeps_future_state_and_health_values() {
        let inspected: RawContainerInspect = serde_json::from_value(json!({
            "State": {
                "Status": "future-state",
                "ExitCode": 7,
                "Health": { "Status": "future-health" }
            }
        }))
        .unwrap();
        assert!(matches!(
            runtime_observation(inspected.state.as_ref()),
            ContainerRuntimeObservation::Unknown { raw }
                if raw == json!({
                    "Status": "future-state",
                    "ExitCode": 7,
                    "Health": { "Status": "future-health" }
                })
        ));
    }
}
