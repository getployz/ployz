use ployz_core::dataplane::{
    EbpfForwardingReady, EbpfForwardingReadyEvidence, PloyzNativeMeshReady,
    WireGuardEbpfPrepareError, WireGuardPublicKey, WireGuardReady, WireGuardReadyEvidence,
};
use ployz_core::ids::{ContainerId, MachineId};
use ployz_core::machine_runtime::ManagedContainerIdentity;
use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, ManagedContainerObservation,
};
use ployzd::roles::machine::runner::{
    CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
    MachineContainerRunner, MachineContainerRunnerError, MachineLogReader, MachineLogReaderError,
    MachineLogTail,
};
use ployzd::roles::machine::service::MachinePloyzNativeMeshPreparer;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct ObservingContainerRunner {
    machine_id: MachineId,
    state: Arc<Mutex<ObservingContainerRunnerState>>,
}

impl ObservingContainerRunner {
    #[must_use]
    pub fn new(machine_id: MachineId) -> Self {
        let snapshot = empty_snapshot(&machine_id);
        Self {
            machine_id,
            state: Arc::new(Mutex::new(ObservingContainerRunnerState {
                next_container_number: 0,
                snapshot,
            })),
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> MachineContainerObservationSnapshot {
        self.state
            .lock()
            .expect("observing runner state lock is not poisoned")
            .snapshot
            .clone()
    }

    fn replace_snapshot(&self, snapshot: MachineContainerObservationSnapshot) {
        self.state
            .lock()
            .expect("observing runner state lock is not poisoned")
            .snapshot = snapshot;
    }
}

impl MachineContainerRunner for ObservingContainerRunner {
    async fn existing_managed_containers(
        &self,
    ) -> Result<Vec<ExistingManagedContainer>, MachineContainerRunnerError> {
        Ok(self
            .snapshot()
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
            identity: command.identity,
            state: ContainerRuntimeState::Exited,
            health_status: None,
            resolved_image_identity: None,
            created_at_unix_seconds: None,
        };
        let snapshot = self
            .snapshot()
            .with_container_replaced(observation)
            .map_err(|error| MachineContainerRunnerError::Create {
                message: error.to_string(),
            })?;
        self.replace_snapshot(snapshot);

        Ok(container_id)
    }

    async fn start_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> Result<(), MachineContainerRunnerError> {
        let snapshot = self.snapshot();
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
        self.replace_snapshot(snapshot);
        Ok(())
    }

    async fn wait_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> Result<i64, MachineContainerRunnerError> {
        let snapshot = self.snapshot();
        let Some(observation) = snapshot.container(container_id).cloned() else {
            return Err(MachineContainerRunnerError::Wait {
                container_id: container_id.clone(),
                message: "container not found".to_owned(),
            });
        };
        let snapshot = snapshot
            .with_container_replaced(ManagedContainerObservation {
                state: ContainerRuntimeState::Exited,
                ..observation
            })
            .map_err(|error| MachineContainerRunnerError::Wait {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
        self.replace_snapshot(snapshot);
        Ok(0)
    }

    async fn remove_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> Result<(), MachineContainerRunnerError> {
        let snapshot = self.snapshot();

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
        self.replace_snapshot(snapshot);
        Ok(())
    }

    async fn remove_volume(
        &self,
        _docker_volume_name: &str,
    ) -> Result<(), MachineContainerRunnerError> {
        Ok(())
    }

    async fn stop_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> Result<(), MachineContainerRunnerError> {
        let snapshot = self.snapshot();

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
        self.replace_snapshot(snapshot);
        Ok(())
    }

    async fn restart_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> Result<(), MachineContainerRunnerError> {
        let snapshot = self.snapshot();

        let Some(existing) = snapshot.container(container_id).cloned() else {
            return Err(MachineContainerRunnerError::Restart {
                container_id: container_id.clone(),
                message: "container was not found".to_owned(),
            });
        };
        if observation_identity(&existing) != *expected_identity {
            return Err(MachineContainerRunnerError::Restart {
                container_id: container_id.clone(),
                message: "container identity did not match restart target".to_owned(),
            });
        }

        let snapshot = snapshot
            .with_container_replaced(ManagedContainerObservation {
                state: ContainerRuntimeState::running_at(std::net::Ipv4Addr::LOCALHOST.into()),
                ..existing
            })
            .map_err(|error| MachineContainerRunnerError::Restart {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
        self.replace_snapshot(snapshot);
        Ok(())
    }
}

impl MachineLogReader for ObservingContainerRunner {
    async fn tail_container_logs(
        &self,
        container_id: &ContainerId,
        _query: ployzd::roles::machine::runner::MachineLogQuery,
    ) -> Result<MachineLogTail, MachineLogReaderError> {
        let snapshot = self.snapshot();
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
    observation.identity.clone()
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

#[derive(Debug)]
struct ObservingContainerRunnerState {
    next_container_number: u64,
    snapshot: MachineContainerObservationSnapshot,
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
    async fn read_ployz_native_mesh_status(
        &self,
        _mode: ployz_core::dataplane::NetworkStatusMode,
    ) -> Result<ployz_core::dataplane::MachineDataplaneStatus, String> {
        Err("dataplane status is unavailable".to_owned())
    }

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

    async fn prepare_wireguard(
        &self,
        endpoint_routes: &[ployz_core::dataplane::WireGuardEbpfEndpointRoute],
        peers: &[ployz_core::dataplane::WireGuardPeer],
    ) -> Result<WireGuardReady, WireGuardEbpfPrepareError> {
        self.prepare_ployz_native_mesh(endpoint_routes, peers)
            .await
            .map(|ready| ready.wireguard)
    }

    async fn probe_overlay(
        &self,
        _peers: &[ployz_core::dataplane::WireGuardPublicKey],
    ) -> Result<Vec<ployz_core::dataplane::WireGuardPublicKey>, WireGuardEbpfPrepareError> {
        Ok(Vec::new())
    }

    async fn probe_link_mtu(
        &self,
        _peer_gateway: std::net::Ipv4Addr,
    ) -> Result<u32, WireGuardEbpfPrepareError> {
        Ok(1380)
    }
}

fn existing_container_from_observation(
    observation: &ManagedContainerObservation,
) -> ExistingManagedContainer {
    ExistingManagedContainer {
        container_id: observation.container_id.clone(),
        identity: observation.identity.clone(),
        state: existing_container_state(&observation.state),
        health_status: observation.health_status,
        resolved_image_identity: observation.resolved_image_identity.clone(),
        created_at_unix_seconds: observation.created_at_unix_seconds,
    }
}

fn existing_container_state(state: &ContainerRuntimeState) -> ExistingManagedContainerState {
    match state {
        ContainerRuntimeState::Running { ip, health } => ExistingManagedContainerState::Running {
            ip: *ip,
            health: *health,
        },
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
        message: "container is missing from observed runner state".to_owned(),
    }
}
