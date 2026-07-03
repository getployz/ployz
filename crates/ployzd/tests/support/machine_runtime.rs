use ployz_core::dataplane::{
    EbpfForwardingReady, EbpfForwardingReadyEvidence, PloyzNativeMeshReady,
    WireGuardEbpfPrepareError, WireGuardPublicKey, WireGuardReady, WireGuardReadyEvidence,
};
use ployz_core::ids::{ContainerId, MachineId};
use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot,
    ManagedContainerObservation,
};
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
use ployzd::docker::labels::{ManagedContainerIdentity, ManagedContainerLabels};
use ployzd::machine_runtime::runner::{
    CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
    MachineContainerRunner, MachineContainerRunnerError, MachineLogReader, MachineLogReaderError,
    MachineLogTail,
};
use ployzd::machine_runtime::service::MachinePloyzNativeMeshPreparer;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct ObservingContainerRunner {
    machine_id: MachineId,
    observations: AsyncNatsObservationStore,
    state: Arc<Mutex<ObservingContainerRunnerState>>,
}

impl ObservingContainerRunner {
    #[must_use]
    pub fn new(machine_id: MachineId, observations: AsyncNatsObservationStore) -> Self {
        Self {
            machine_id,
            observations,
            state: Arc::new(Mutex::new(ObservingContainerRunnerState::default())),
        }
    }
}

impl MachineContainerRunner for ObservingContainerRunner {
    async fn existing_managed_containers(
        &self,
    ) -> Result<Vec<ExistingManagedContainer>, MachineContainerRunnerError> {
        let Some(snapshot) = self
            .observations
            .machine_snapshot(&self.machine_id)
            .await
            .map_err(machine_observation_list_error)?
        else {
            return Ok(Vec::new());
        };

        Ok(snapshot
            .containers()
            .iter()
            .map(existing_container_from_observation)
            .collect())
    }

    async fn ensure_endpoint_network(&self) -> Result<(), MachineContainerRunnerError> {
        Ok(())
    }

    async fn create_managed_container(
        &self,
        command: CreateManagedContainer,
    ) -> Result<ContainerId, MachineContainerRunnerError> {
        let container_id = self.next_container_id()?;
        let observation = ManagedContainerObservation {
            machine_id: self.machine_id.clone(),
            container_id: container_id.clone(),
            namespace_id: command.labels.namespace_id,
            service_id: command.labels.service_id,
            namespace_revision_entry_id: command.labels.namespace_revision_entry_id,
            operation_id: command.labels.operation_id,
            step_id: command.labels.step_id,
            kind: command.labels.kind,
            state: ContainerRuntimeState::Exited,
        };
        let snapshot = self
            .observations
            .machine_snapshot(&self.machine_id)
            .await
            .map_err(machine_observation_create_error)?
            .unwrap_or_else(|| empty_snapshot(&self.machine_id))
            .with_container_replaced(observation)
            .map_err(|error| MachineContainerRunnerError::Create {
                message: error.to_string(),
            })?;
        self.observations
            .replace_machine_containers(&snapshot)
            .await
            .map_err(machine_observation_create_error)?;

        Ok(container_id)
    }

    async fn start_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> Result<(), MachineContainerRunnerError> {
        let Some(snapshot) = self
            .observations
            .machine_snapshot(&self.machine_id)
            .await
            .map_err(machine_observation_create_error)?
        else {
            return Err(missing_container_start_error(container_id));
        };
        let Some(observation) = snapshot.container(container_id).cloned() else {
            return Err(missing_container_start_error(container_id));
        };
        // Every container joins the endpoint network at creation (ADR 0023),
        // so a started container always observes an endpoint IP.
        let snapshot = snapshot
            .with_container_replaced(ManagedContainerObservation {
                state: ContainerRuntimeState::running_at(std::net::Ipv4Addr::LOCALHOST.into()),
                ..observation
            })
            .map_err(|error| MachineContainerRunnerError::Start {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
        self.observations
            .replace_machine_containers(&snapshot)
            .await
            .map_err(|error| MachineContainerRunnerError::Start {
                container_id: container_id.clone(),
                message: error.to_string(),
            })
    }

    async fn remove_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> Result<(), MachineContainerRunnerError> {
        let Some(snapshot) = self
            .observations
            .machine_snapshot(&self.machine_id)
            .await
            .map_err(|error| machine_observation_remove_error(container_id, error))?
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
            return Err(MachineContainerRunnerError::Remove {
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
        let snapshot =
            MachineContainerObservationSnapshot::try_new(self.machine_id.clone(), containers)
                .map_err(|error| MachineContainerRunnerError::Remove {
                    container_id: container_id.clone(),
                    message: error.to_string(),
                })?;
        self.observations
            .replace_machine_containers(&snapshot)
            .await
            .map_err(|error| machine_observation_remove_error(container_id, error))
    }

    async fn stop_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> Result<(), MachineContainerRunnerError> {
        let Some(snapshot) = self
            .observations
            .machine_snapshot(&self.machine_id)
            .await
            .map_err(|error| machine_observation_stop_error(container_id, error))?
        else {
            return Ok(());
        };

        let Some(existing) = snapshot.container(container_id).cloned() else {
            return Ok(());
        };
        if observation_identity(&existing) != *expected_identity {
            return Err(MachineContainerRunnerError::Stop {
                container_id: container_id.clone(),
                message: "container identity did not match stop target".to_owned(),
            });
        }

        let snapshot = snapshot
            .with_container_replaced(ManagedContainerObservation {
                state: ContainerRuntimeState::Exited,
                ..existing
            })
            .map_err(|error| MachineContainerRunnerError::Stop {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
        self.observations
            .replace_machine_containers(&snapshot)
            .await
            .map_err(|error| machine_observation_stop_error(container_id, error))
    }
}

impl MachineLogReader for ObservingContainerRunner {
    async fn tail_container_logs(
        &self,
        container_id: &ContainerId,
        _tail_lines: Option<u16>,
    ) -> Result<MachineLogTail, MachineLogReaderError> {
        let Some(snapshot) = self
            .observations
            .machine_snapshot(&self.machine_id)
            .await
            .map_err(|error| MachineLogReaderError::ReadFailed {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?
        else {
            return Err(MachineLogReaderError::NotFound {
                container_id: container_id.clone(),
            });
        };
        if snapshot.container(container_id).is_none() {
            return Err(MachineLogReaderError::NotFound {
                container_id: container_id.clone(),
            });
        }

        Ok(MachineLogTail {
            text: format!("logs for {}\n", container_id.as_str()),
            truncated: false,
        })
    }
}

fn observation_identity(observation: &ManagedContainerObservation) -> ManagedContainerIdentity {
    ManagedContainerIdentity {
        namespace_id: observation.namespace_id.clone(),
        service_id: observation.service_id.clone(),
        namespace_revision_entry_id: observation.namespace_revision_entry_id.clone(),
        operation_id: observation.operation_id.clone(),
        step_id: observation.step_id.clone(),
        kind: observation.kind,
    }
}

impl ObservingContainerRunner {
    fn next_container_id(&self) -> Result<ContainerId, MachineContainerRunnerError> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| MachineContainerRunnerError::Create {
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
    fn next_container_id(&mut self) -> Result<ContainerId, MachineContainerRunnerError> {
        self.next_container_number += 1;
        ContainerId::try_new(format!("ctr_{}", self.next_container_number)).map_err(|error| {
            MachineContainerRunnerError::Create {
                message: error.to_string(),
            }
        })
    }
}

#[derive(Debug, Clone)]
pub struct ReadyWireGuardEbpf;

impl MachinePloyzNativeMeshPreparer for ReadyWireGuardEbpf {
    async fn read_wireguard_public_key(
        &self,
    ) -> Result<WireGuardPublicKey, WireGuardEbpfPrepareError> {
        Ok(WireGuardPublicKey::try_new("test-public-key").expect("test public key is valid"))
    }

    async fn prepare_ployz_native_mesh(
        &self,
        _endpoint_routes: &[ployz_core::dataplane::WireGuardEbpfEndpointRoute],
        _peers: &[ployz_core::dataplane::WireGuardPeer],
    ) -> Result<PloyzNativeMeshReady, WireGuardEbpfPrepareError> {
        Ok(PloyzNativeMeshReady {
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
            namespace_id: observation.namespace_id.clone(),
            service_id: observation.service_id.clone(),
            namespace_revision_entry_id: observation.namespace_revision_entry_id.clone(),
            operation_id: observation.operation_id.clone(),
            step_id: observation.step_id.clone(),
            kind: observation.kind,
        },
        state: existing_container_state(&observation.state),
    }
}

fn existing_container_state(state: &ContainerRuntimeState) -> ExistingManagedContainerState {
    match state {
        ContainerRuntimeState::Running { ip } => ExistingManagedContainerState::Running { ip: *ip },
        ContainerRuntimeState::Exited => ExistingManagedContainerState::StartableStopped,
    }
}

fn empty_snapshot(machine_id: &MachineId) -> MachineContainerObservationSnapshot {
    MachineContainerObservationSnapshot::try_new(machine_id.clone(), Vec::new())
        .expect("empty machine snapshot is valid")
}

fn missing_container_start_error(container_id: &ContainerId) -> MachineContainerRunnerError {
    MachineContainerRunnerError::Start {
        container_id: container_id.clone(),
        message: "container is missing from observations".to_owned(),
    }
}

fn machine_observation_list_error(error: ObservationStoreError) -> MachineContainerRunnerError {
    MachineContainerRunnerError::ListExisting {
        message: error.to_string(),
    }
}

fn machine_observation_create_error(error: ObservationStoreError) -> MachineContainerRunnerError {
    MachineContainerRunnerError::Create {
        message: error.to_string(),
    }
}

fn machine_observation_remove_error(
    container_id: &ContainerId,
    error: ObservationStoreError,
) -> MachineContainerRunnerError {
    MachineContainerRunnerError::Remove {
        container_id: container_id.clone(),
        message: error.to_string(),
    }
}

fn machine_observation_stop_error(
    container_id: &ContainerId,
    error: ObservationStoreError,
) -> MachineContainerRunnerError {
    MachineContainerRunnerError::Stop {
        container_id: container_id.clone(),
        message: error.to_string(),
    }
}
