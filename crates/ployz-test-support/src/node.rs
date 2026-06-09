use ployz_core::dataplane::{
    EbpfForwardingReady, EbpfForwardingReadyEvidence, WireGuardEbpfPrepareError,
    WireGuardEbpfReady, WireGuardPublicKey, WireGuardReady, WireGuardReadyEvidence,
};
use ployz_core::ids::{ContainerId, NodeId};
use ployz_core::node::{
    ContainerEndpoint, ContainerRuntimeState, ManagedContainerObservation,
    NodeContainerObservationSnapshot,
};
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
use ployzd::docker::labels::{ManagedContainerIdentity, ManagedContainerLabels};
use ployzd::node_agent::runtime::{
    CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
    NodeContainerRunner, NodeContainerRunnerError, NodeLogReader, NodeLogReaderError, NodeLogTail,
};
use ployzd::node_runtime_types::ContainerEndpointRequest;
use ployzd::node_service_runtime::NodeWireGuardEbpfPreparer;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct ObservingContainerRunner {
    node_id: NodeId,
    observations: AsyncNatsObservationStore,
    state: Arc<Mutex<ObservingContainerRunnerState>>,
}

impl ObservingContainerRunner {
    #[must_use]
    pub fn new(node_id: NodeId, observations: AsyncNatsObservationStore) -> Self {
        Self {
            node_id,
            observations,
            state: Arc::new(Mutex::new(ObservingContainerRunnerState::default())),
        }
    }
}

impl NodeContainerRunner for ObservingContainerRunner {
    async fn existing_managed_containers(
        &self,
    ) -> Result<Vec<ExistingManagedContainer>, NodeContainerRunnerError> {
        let Some(snapshot) = self
            .observations
            .node_snapshot(&self.node_id)
            .await
            .map_err(node_observation_list_error)?
        else {
            return Ok(Vec::new());
        };

        Ok(snapshot
            .containers()
            .iter()
            .map(existing_container_from_observation)
            .collect())
    }

    async fn create_managed_container(
        &self,
        command: CreateManagedContainer,
    ) -> Result<ContainerId, NodeContainerRunnerError> {
        let container_id = self.next_container_id()?;
        self.store_endpoint(&container_id, command.endpoint)?;
        let observation = ManagedContainerObservation {
            node_id: self.node_id.clone(),
            container_id: container_id.clone(),
            service_id: command.labels.service_id,
            revision_id: command.labels.revision_id,
            operation_id: command.labels.operation_id,
            step_id: command.labels.step_id,
            kind: command.labels.kind,
            state: ContainerRuntimeState::Exited,
        };
        let snapshot = self
            .observations
            .node_snapshot(&self.node_id)
            .await
            .map_err(node_observation_create_error)?
            .unwrap_or_else(|| empty_snapshot(&self.node_id))
            .with_container_replaced(observation)
            .map_err(|error| NodeContainerRunnerError::Create {
                message: error.to_string(),
            })?;
        self.observations
            .replace_node_containers(&snapshot)
            .await
            .map_err(node_observation_create_error)?;

        Ok(container_id)
    }

    async fn start_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> Result<(), NodeContainerRunnerError> {
        let Some(snapshot) = self
            .observations
            .node_snapshot(&self.node_id)
            .await
            .map_err(node_observation_create_error)?
        else {
            return Err(missing_container_start_error(container_id));
        };
        let Some(observation) = snapshot.container(container_id).cloned() else {
            return Err(missing_container_start_error(container_id));
        };
        let endpoint = self.endpoint_for(container_id)?;
        let snapshot = snapshot
            .with_container_replaced(ManagedContainerObservation {
                state: endpoint.map_or_else(
                    ContainerRuntimeState::running_unroutable,
                    ContainerRuntimeState::running_at,
                ),
                ..observation
            })
            .map_err(|error| NodeContainerRunnerError::Start {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
        self.observations
            .replace_node_containers(&snapshot)
            .await
            .map_err(|error| NodeContainerRunnerError::Start {
                container_id: container_id.clone(),
                message: error.to_string(),
            })
    }

    async fn remove_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> Result<(), NodeContainerRunnerError> {
        let Some(snapshot) = self
            .observations
            .node_snapshot(&self.node_id)
            .await
            .map_err(|error| node_observation_remove_error(container_id, error))?
        else {
            return Ok(());
        };

        let existing = snapshot
            .containers()
            .iter()
            .find(|container| container.container_id == *container_id);
        let Some(existing) = existing else {
            return Ok(());
        };
        if observation_identity(existing) != *expected_identity {
            return Err(NodeContainerRunnerError::Remove {
                container_id: container_id.clone(),
                message: "container identity did not match cleanup target".to_owned(),
            });
        }

        let containers = snapshot
            .containers()
            .iter()
            .filter(|container| container.container_id != *container_id)
            .cloned()
            .collect::<Vec<_>>();
        let snapshot = NodeContainerObservationSnapshot::try_new(self.node_id.clone(), containers)
            .map_err(|error| NodeContainerRunnerError::Remove {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
        self.observations
            .replace_node_containers(&snapshot)
            .await
            .map_err(|error| node_observation_remove_error(container_id, error))
    }

    async fn stop_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> Result<(), NodeContainerRunnerError> {
        let Some(snapshot) = self
            .observations
            .node_snapshot(&self.node_id)
            .await
            .map_err(|error| node_observation_stop_error(container_id, error))?
        else {
            return Ok(());
        };

        let Some(existing) = snapshot.container(container_id).cloned() else {
            return Ok(());
        };
        if observation_identity(&existing) != *expected_identity {
            return Err(NodeContainerRunnerError::Stop {
                container_id: container_id.clone(),
                message: "container identity did not match stop target".to_owned(),
            });
        }

        let snapshot = snapshot
            .with_container_replaced(ManagedContainerObservation {
                state: ContainerRuntimeState::Exited,
                ..existing
            })
            .map_err(|error| NodeContainerRunnerError::Stop {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
        self.observations
            .replace_node_containers(&snapshot)
            .await
            .map_err(|error| node_observation_stop_error(container_id, error))
    }
}

impl NodeLogReader for ObservingContainerRunner {
    async fn tail_container_logs(
        &self,
        container_id: &ContainerId,
        _tail_lines: Option<u16>,
    ) -> Result<NodeLogTail, NodeLogReaderError> {
        let Some(snapshot) = self
            .observations
            .node_snapshot(&self.node_id)
            .await
            .map_err(|error| NodeLogReaderError::ReadFailed {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?
        else {
            return Err(NodeLogReaderError::NotFound {
                container_id: container_id.clone(),
            });
        };
        if snapshot.container(container_id).is_none() {
            return Err(NodeLogReaderError::NotFound {
                container_id: container_id.clone(),
            });
        }

        Ok(NodeLogTail {
            text: format!("logs for {}\n", container_id.as_str()),
            truncated: false,
        })
    }
}

fn observation_identity(observation: &ManagedContainerObservation) -> ManagedContainerIdentity {
    ManagedContainerIdentity {
        service_id: observation.service_id.clone(),
        revision_id: observation.revision_id.clone(),
        operation_id: observation.operation_id.clone(),
        step_id: observation.step_id.clone(),
        kind: observation.kind,
    }
}

impl ObservingContainerRunner {
    fn next_container_id(&self) -> Result<ContainerId, NodeContainerRunnerError> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| NodeContainerRunnerError::Create {
                message: error.to_string(),
            })?;
        state.next_container_id()
    }

    fn store_endpoint(
        &self,
        container_id: &ContainerId,
        endpoint: Option<ContainerEndpointRequest>,
    ) -> Result<(), NodeContainerRunnerError> {
        let Some(endpoint) = endpoint else {
            return Ok(());
        };
        let mut state = self
            .state
            .lock()
            .map_err(|error| NodeContainerRunnerError::Create {
                message: error.to_string(),
            })?;
        state.endpoints.push((
            container_id.clone(),
            ContainerEndpoint {
                ip: std::net::Ipv4Addr::LOCALHOST.into(),
                port: endpoint.port,
            },
        ));
        Ok(())
    }

    fn endpoint_for(
        &self,
        container_id: &ContainerId,
    ) -> Result<Option<ContainerEndpoint>, NodeContainerRunnerError> {
        let state = self
            .state
            .lock()
            .map_err(|error| NodeContainerRunnerError::Start {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
        Ok(state
            .endpoints
            .iter()
            .find(|(id, _)| id == container_id)
            .map(|(_, endpoint)| endpoint.clone()))
    }
}

#[derive(Debug, Default)]
struct ObservingContainerRunnerState {
    next_container_number: u64,
    endpoints: Vec<(ContainerId, ContainerEndpoint)>,
}

impl ObservingContainerRunnerState {
    fn next_container_id(&mut self) -> Result<ContainerId, NodeContainerRunnerError> {
        self.next_container_number += 1;
        ContainerId::try_new(format!("ctr_{}", self.next_container_number)).map_err(|error| {
            NodeContainerRunnerError::Create {
                message: error.to_string(),
            }
        })
    }
}

#[derive(Debug, Clone)]
pub struct ReadyWireGuardEbpf;

impl NodeWireGuardEbpfPreparer for ReadyWireGuardEbpf {
    async fn read_wireguard_public_key(
        &self,
    ) -> Result<WireGuardPublicKey, WireGuardEbpfPrepareError> {
        Ok(WireGuardPublicKey::try_new("test-public-key").expect("test public key is valid"))
    }

    async fn prepare_wireguard_ebpf(
        &self,
        _endpoint_routes: &[ployz_core::dataplane::WireGuardEbpfEndpointRoute],
        _peers: &[ployz_core::dataplane::WireGuardPeer],
    ) -> Result<WireGuardEbpfReady, WireGuardEbpfPrepareError> {
        Ok(WireGuardEbpfReady {
            wireguard: WireGuardReady {
                public_key: WireGuardPublicKey::try_new("test-public-key")
                    .expect("test public key is valid"),
                evidence: vec![WireGuardReadyEvidence::Command {
                    program: "wg".to_owned(),
                    args: vec!["--version".to_owned()],
                }],
            },
            ebpf_forwarding: EbpfForwardingReady {
                evidence: vec![EbpfForwardingReadyEvidence::PloyzTcBytecode {
                    path: "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".to_owned(),
                    symbols: vec!["ployz_egress".to_owned(), "ployz_ingress".to_owned()],
                }],
            },
        })
    }
}

fn existing_container_from_observation(
    observation: &ManagedContainerObservation,
) -> ExistingManagedContainer {
    ExistingManagedContainer {
        container_id: observation.container_id.clone(),
        labels: ManagedContainerLabels {
            service_id: observation.service_id.clone(),
            revision_id: observation.revision_id.clone(),
            operation_id: observation.operation_id.clone(),
            step_id: observation.step_id.clone(),
            kind: observation.kind,
            endpoint_port: observation
                .running_service_endpoint()
                .map(|endpoint| endpoint.port),
        },
        state: existing_container_state(&observation.state),
    }
}

fn existing_container_state(state: &ContainerRuntimeState) -> ExistingManagedContainerState {
    match state {
        ContainerRuntimeState::Running { endpoint } => ExistingManagedContainerState::Running {
            endpoint: endpoint.clone(),
        },
        ContainerRuntimeState::Exited => ExistingManagedContainerState::StartableStopped,
    }
}

fn empty_snapshot(node_id: &NodeId) -> NodeContainerObservationSnapshot {
    NodeContainerObservationSnapshot::try_new(node_id.clone(), Vec::new())
        .expect("empty node snapshot is valid")
}

fn missing_container_start_error(container_id: &ContainerId) -> NodeContainerRunnerError {
    NodeContainerRunnerError::Start {
        container_id: container_id.clone(),
        message: "container is missing from observations".to_owned(),
    }
}

fn node_observation_list_error(error: ObservationStoreError) -> NodeContainerRunnerError {
    NodeContainerRunnerError::ListExisting {
        message: error.to_string(),
    }
}

fn node_observation_create_error(error: ObservationStoreError) -> NodeContainerRunnerError {
    NodeContainerRunnerError::Create {
        message: error.to_string(),
    }
}

fn node_observation_remove_error(
    container_id: &ContainerId,
    error: ObservationStoreError,
) -> NodeContainerRunnerError {
    NodeContainerRunnerError::Remove {
        container_id: container_id.clone(),
        message: error.to_string(),
    }
}

fn node_observation_stop_error(
    container_id: &ContainerId,
    error: ObservationStoreError,
) -> NodeContainerRunnerError {
    NodeContainerRunnerError::Stop {
        container_id: container_id.clone(),
        message: error.to_string(),
    }
}
