//! Periodic publication of machine-owned service endpoint testimony.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use ployz_core::corrosion::{
    CorrosionDocumentVersion, CorrosionTimestamp, MachineEndpointDocument, ServiceEndpoint,
    SqliteParameter, Statement, TransactionResult,
};
use ployz_core::ids::{ClusterName, MachineName};
use ployz_core::network::EndpointBridgeStatus;
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;

use crate::corrosion::{CorrosionClient, CorrosionClientError};
use crate::roles::api::execution::docker::runner::DockerManagedContainerRunner;
use crate::roles::api::runner::{
    ExistingManagedContainerState, ExistingV2ManagedContainer, V2MachineContainerRunner,
};

const REPORT_INTERVAL: Duration = Duration::from_secs(5);
const DOCKER_OBSERVE_TIMEOUT: Duration = Duration::from_secs(10);

/// Publishes one complete machine-owned view. Missing endpoints therefore
/// disappear on the next successful report without per-container deletes.
pub(super) struct MachineEndpointReporter {
    client: CorrosionClient,
    cluster_id: ClusterName,
    local_machine_id: MachineName,
    runtime: Arc<DockerManagedContainerRunner>,
}

impl MachineEndpointReporter {
    #[must_use]
    pub(super) const fn new(
        client: CorrosionClient,
        cluster_id: ClusterName,
        local_machine_id: MachineName,
        runtime: Arc<DockerManagedContainerRunner>,
    ) -> Self {
        Self {
            client,
            cluster_id,
            local_machine_id,
            runtime,
        }
    }

    /// Reports immediately, then periodically until API shutdown. A non-ready
    /// endpoint network publishes an empty serving view. Container observation
    /// or Corrosion outages leave the prior testimony visible with its old
    /// `observed_at`; the next successful cycle replaces it in one write.
    pub(super) async fn run(self, mut shutdown: watch::Receiver<bool>) {
        let mut interval = tokio::time::interval(REPORT_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                }
                _ = interval.tick() => {
                    if let Err(error) = self.publish().await {
                        tracing::warn!(error = %error, "could not publish local service endpoint testimony");
                    }
                }
            }
        }
    }

    async fn publish(&self) -> Result<(), MachineEndpointPublishError> {
        let cluster = super::store::read_cluster(&self.client, &self.cluster_id)
            .await
            .map_err(|error| MachineEndpointPublishError::Roster {
                detail: error.to_string(),
            })?;
        let machine =
            super::store::read_machine(&self.client, &cluster.document, &self.local_machine_id)
                .await
                .map_err(|error| MachineEndpointPublishError::Roster {
                    detail: error.to_string(),
                })?;
        if machine.is_none() {
            return Ok(());
        }
        let endpoint_network_ready = matches!(
            tokio::time::timeout(
                DOCKER_OBSERVE_TIMEOUT,
                self.runtime.read_endpoint_network_status(),
            )
            .await,
            Ok(EndpointBridgeStatus::Ready { .. })
        );
        let containers = if endpoint_network_ready {
            tokio::time::timeout(
                DOCKER_OBSERVE_TIMEOUT,
                self.runtime.existing_v2_managed_containers(),
            )
            .await
            .map_err(|_| MachineEndpointPublishError::Docker {
                detail: "observation timed out".to_owned(),
            })?
            .map_err(|error| MachineEndpointPublishError::Docker {
                detail: format!("{error:?}"),
            })?
        } else {
            Vec::new()
        };
        let document = document(
            self.cluster_id.clone(),
            self.local_machine_id.clone(),
            CorrosionTimestamp::now_utc(),
            endpoint_network_ready,
            containers,
        );
        let response = self.client.execute(&[statement(&document)?]).await?;
        let [TransactionResult::Success(result)] = response.results.as_slice() else {
            return Err(MachineEndpointPublishError::UnexpectedWriteResult);
        };
        if result.rows_affected != 1 {
            return Err(MachineEndpointPublishError::UnexpectedRowsAffected {
                rows_affected: result.rows_affected,
            });
        }
        Ok(())
    }
}

fn document(
    cluster_id: ClusterName,
    machine_id: MachineName,
    observed_at: CorrosionTimestamp,
    endpoint_network_ready: bool,
    containers: Vec<ExistingV2ManagedContainer>,
) -> MachineEndpointDocument {
    let mut endpoints = if endpoint_network_ready {
        containers
            .into_iter()
            .filter_map(|container| {
                let ExistingManagedContainerState::Running {
                    ip: Some(IpAddr::V4(ip)),
                    ..
                } = container.state
                else {
                    return None;
                };
                Some(ServiceEndpoint {
                    namespace_id: container.identity.namespace_id,
                    service_name: container.identity.service_name,
                    deploy: container.identity.operation_id,
                    replica_slot: container.identity.replica_slot,
                    ip,
                })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    endpoints.sort_by(|left, right| {
        (
            &left.namespace_id,
            &left.service_name,
            &left.deploy,
            left.replica_slot,
            left.ip,
        )
            .cmp(&(
                &right.namespace_id,
                &right.service_name,
                &right.deploy,
                right.replica_slot,
                right.ip,
            ))
    });
    MachineEndpointDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id,
        machine_id,
        observed_at,
        endpoints,
    }
}

fn statement(document: &MachineEndpointDocument) -> Result<Statement, MachineEndpointPublishError> {
    let encoded =
        serde_json::to_string(document).map_err(|error| MachineEndpointPublishError::Encode {
            detail: error.to_string(),
        })?;
    Ok(Statement::with_params(
        "INSERT INTO machine_endpoints (id, document) VALUES (?, ?) \
         ON CONFLICT(id) DO UPDATE SET document = excluded.document",
        vec![
            SqliteParameter::Text(document.machine_id.as_str().to_owned()),
            SqliteParameter::Text(encoded),
        ],
    ))
}

#[derive(Debug, thiserror::Error)]
enum MachineEndpointPublishError {
    #[error(transparent)]
    Corrosion(#[from] CorrosionClientError),
    #[error("could not read the local accepted roster: {detail}")]
    Roster { detail: String },
    #[error("could not list local managed containers: {detail}")]
    Docker { detail: String },
    #[error("could not encode endpoint testimony: {detail}")]
    Encode { detail: String },
    #[error("endpoint testimony write returned an unexpected result")]
    UnexpectedWriteResult,
    #[error("endpoint testimony write affected {rows_affected} rows instead of one")]
    UnexpectedRowsAffected { rows_affected: usize },
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use ployz_core::corrosion::{HostPortBindings, V2ManagedContainerIdentity};
    use ployz_core::deploy::{ReplicaSlot, ReplicatedReplicaSlot};
    use ployz_core::ids::{ContainerId, CorrosionNamespaceName, DeployName};
    use ployz_core::machine::runtime::{ContainerHealth, ManagedContainerHealthStatus};

    use super::*;

    #[test]
    fn complete_document_contains_only_running_ipv4_endpoints_without_runtime_ids() {
        let document = document(
            cluster(),
            machine(),
            timestamp(),
            true,
            vec![
                container(
                    "docker-running-v4",
                    "web",
                    ExistingManagedContainerState::Running {
                        ip: Some(IpAddr::V4(Ipv4Addr::new(10, 210, 4, 8))),
                        health: ContainerHealth::Healthy,
                        started_at_unix_ms: Some(7),
                    },
                ),
                container(
                    "docker-stopped",
                    "worker",
                    ExistingManagedContainerState::StartableStopped,
                ),
                container(
                    "docker-running-v6",
                    "metrics",
                    ExistingManagedContainerState::Running {
                        ip: Some(IpAddr::V6(Ipv6Addr::LOCALHOST)),
                        health: ContainerHealth::Healthy,
                        started_at_unix_ms: Some(8),
                    },
                ),
                container(
                    "docker-running-no-ip",
                    "admin",
                    ExistingManagedContainerState::Running {
                        ip: None,
                        health: ContainerHealth::Healthy,
                        started_at_unix_ms: Some(9),
                    },
                ),
            ],
        );

        assert_eq!(document.endpoints.len(), 1);
        let endpoint = document.endpoints.first().expect("reported endpoint");
        assert_eq!(endpoint.service_name.as_str(), "web");
        assert_eq!(endpoint.ip, Ipv4Addr::new(10, 210, 4, 8));
        let encoded = serde_json::to_string(&document).expect("endpoint document");
        assert!(!encoded.contains("docker-"));
        assert!(!encoded.contains("runtime_id"));
        assert!(!encoded.contains("container_id"));
    }

    #[test]
    fn non_ready_endpoint_network_suppresses_all_serving_testimony() {
        let document = document(
            cluster(),
            machine(),
            timestamp(),
            false,
            vec![container(
                "docker-running-v4",
                "web",
                ExistingManagedContainerState::Running {
                    ip: Some(IpAddr::V4(Ipv4Addr::new(10, 211, 4, 8))),
                    health: ContainerHealth::Healthy,
                    started_at_unix_ms: Some(7),
                },
            )],
        );

        assert!(document.endpoints.is_empty());
    }

    #[test]
    fn upsert_uses_the_canonical_machine_name_for_key_and_document() {
        let document = document(cluster(), machine(), timestamp(), true, Vec::new());
        let Statement::WithParams(sql, params) = statement(&document).expect("statement") else {
            panic!("endpoint publication is parameterized");
        };
        assert_eq!(
            sql,
            "INSERT INTO machine_endpoints (id, document) VALUES (?, ?) \
             ON CONFLICT(id) DO UPDATE SET document = excluded.document"
        );
        let [SqliteParameter::Text(key), SqliteParameter::Text(encoded)] = params.as_slice() else {
            panic!("endpoint publication has one key and one document");
        };
        let decoded: MachineEndpointDocument =
            serde_json::from_str(encoded).expect("endpoint document");
        assert_eq!(key, "machine-one");
        assert_eq!(decoded.machine_id.as_str(), key);
        assert!(decoded.endpoints.is_empty());
    }

    fn container(
        runtime_id: &str,
        service_name: &str,
        state: ExistingManagedContainerState,
    ) -> ExistingV2ManagedContainer {
        ExistingV2ManagedContainer {
            container_id: ContainerId::try_new(runtime_id).expect("runtime id"),
            identity: V2ManagedContainerIdentity {
                namespace_id: CorrosionNamespaceName::try_new("shop").expect("namespace"),
                service_name: service_name.parse().expect("service"),
                operation_id: DeployName::try_new("summer").expect("deploy"),
                replica_slot: ReplicaSlot::Replicated {
                    number: ReplicatedReplicaSlot::try_new(1).expect("slot"),
                },
            },
            state,
            health_status: Some(ManagedContainerHealthStatus::Healthy),
            resolved_image_identity: None,
            created_at_unix_seconds: None,
            host_ports: HostPortBindings::default(),
        }
    }

    fn cluster() -> ClusterName {
        ClusterName::try_new("dev-cluster").expect("cluster")
    }

    fn machine() -> MachineName {
        MachineName::try_new("machine-one").expect("machine")
    }

    fn timestamp() -> CorrosionTimestamp {
        CorrosionTimestamp::try_new("2026-08-10T00:00:00Z").expect("timestamp")
    }
}
