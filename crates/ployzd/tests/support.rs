use ployz_core::dataplane::WireGuardEbpfPrepareError;
use ployz_core::ids::{ContainerId, NodeId};
use ployz_core::node::{
    ContainerRuntimeState, ManagedContainerObservation, NodeContainerObservationSnapshot,
};
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
use ployzd::docker::labels::ManagedContainerLabels;
use ployzd::node_agent::runtime::{
    CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
    NodeContainerRunner, NodeContainerRunnerError,
};
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
            return Err(NodeContainerRunnerError::Start {
                container_id: container_id.clone(),
                message: "container is missing from observations".to_owned(),
            });
        };
        let Some(observation) = snapshot.container(container_id).cloned() else {
            return Err(NodeContainerRunnerError::Start {
                container_id: container_id.clone(),
                message: "container is missing from observations".to_owned(),
            });
        };
        let snapshot = snapshot
            .with_container_replaced(ManagedContainerObservation {
                state: ContainerRuntimeState::running_unroutable(),
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
}

#[derive(Debug, Default)]
struct ObservingContainerRunnerState {
    next_container_number: u64,
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
    async fn prepare_wireguard_ebpf(&self) -> Result<(), WireGuardEbpfPrepareError> {
        Ok(())
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
