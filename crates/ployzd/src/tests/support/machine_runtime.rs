use crate::roles::machine::protocol::MachineImagePull;
use crate::roles::machine::runner::{
    CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
    MachineContainerCreateError, MachineContainerListError, MachineContainerRemoveError,
    MachineContainerRestartError, MachineContainerRunner, MachineContainerStartError,
    MachineContainerStopError, MachineContainerWaitError, MachineEndpointNetworkError,
    MachineLogReader, MachineLogReaderError, MachineLogTail, MachineRegistryImageResolveError,
    MachineVolumeRemoveError,
};
use crate::roles::machine::service::MachinePloyzNativeMeshPreparer;
use ployz_core::deploy::{ImageReference, RegistryCredential};
use ployz_core::ids::{ContainerId, MachineId};
use ployz_core::image::OciDigest;
use ployz_core::machine::runtime::ManagedContainerIdentity;
use ployz_core::machine::runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, ManagedContainerObservation,
};
use ployz_core::network::{
    DataplaneProjectionFailure, DataplaneProjectionTestimony, EbpfAttachmentStatus,
    EbpfForwardingReady, EbpfForwardingReadyEvidence, EndpointBridgeStatus, MachineDataplaneStatus,
    MachineEndpointSubnet, NativeDataplaneProjectionStatus, PloyzNativeMeshReady,
    WireGuardConfiguredMtu, WireGuardDetectedMtu, WireGuardEbpfPrepareError,
    WireGuardHandshakeStatus, WireGuardInterfaceMtu, WireGuardMtuProbe, WireGuardPeer,
    WireGuardPeerEndpointSubnet, WireGuardPeerStatus, WireGuardPublicKey, WireGuardReady,
    WireGuardReadyEvidence, WireGuardRttStatus, WireGuardStatus,
};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

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
                endpoint_subnet: None,
                resolutions: Vec::new(),
                pulls: Vec::new(),
                resolution_failure: None,
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

    #[must_use]
    pub fn resolutions(&self) -> Vec<(ImageReference, Option<RegistryCredential>)> {
        self.state
            .lock()
            .expect("observing runner state lock is not poisoned")
            .resolutions
            .clone()
    }

    #[must_use]
    pub fn pulls(&self) -> Vec<MachineImagePull> {
        self.state
            .lock()
            .expect("observing runner state lock is not poisoned")
            .pulls
            .clone()
    }

    pub fn fail_registry_resolution(&self, message: impl Into<String>) {
        self.state
            .lock()
            .expect("observing runner state lock is not poisoned")
            .resolution_failure = Some(message.into());
    }

    fn replace_snapshot(&self, snapshot: MachineContainerObservationSnapshot) {
        self.state
            .lock()
            .expect("observing runner state lock is not poisoned")
            .snapshot = snapshot;
    }
}

impl crate::roles::machine::runner::MachineImageRemovalRunner for ObservingContainerRunner {
    async fn remove_image(
        &self,
        _image_identity: &ployz_core::image::OciDigest,
    ) -> Result<ployz_core::image::ImageRemoveOutcome, String> {
        Ok(ployz_core::image::ImageRemoveOutcome::AlreadyAbsent)
    }
}

impl crate::roles::machine::runner::MachineVolumeUsageReader for ObservingContainerRunner {
    async fn read_volume_usage(
        &self,
        _volume: &ployz_core::intent::VolumePinState,
    ) -> Option<ployz_core::machine::VolumeUsageFacts> {
        None
    }
}

impl MachineContainerRunner for ObservingContainerRunner {
    async fn ensure_volume(
        &self,
        _volume: &ployz_core::intent::VolumePinState,
    ) -> Result<(), ployz_core::machine::VolumeEnsureFailure> {
        Ok(())
    }

    async fn existing_managed_containers(
        &self,
    ) -> Result<Vec<ExistingManagedContainer>, MachineContainerListError> {
        Ok(self
            .snapshot()
            .containers()
            .iter()
            .map(existing_container_from_observation)
            .collect())
    }

    async fn ensure_endpoint_network(&self) -> Result<(), MachineEndpointNetworkError> {
        Ok(())
    }

    async fn ensure_projection_endpoint_network(
        &self,
        expected_subnet: &MachineEndpointSubnet,
    ) -> Result<(), MachineEndpointNetworkError> {
        self.state
            .lock()
            .map_err(|error| MachineEndpointNetworkError::EnsureEndpointNetwork {
                message: error.to_string(),
            })?
            .endpoint_subnet = Some(expected_subnet.clone());
        Ok(())
    }

    async fn read_endpoint_network_status(&self) -> EndpointBridgeStatus {
        self.state
            .lock()
            .expect("observing runner state lock is not poisoned")
            .endpoint_subnet
            .clone()
            .map_or(EndpointBridgeStatus::Missing, |subnet| {
                EndpointBridgeStatus::Ready { subnet }
            })
    }

    async fn resolve_registry_image(
        &self,
        reference: &ImageReference,
        credential: Option<&RegistryCredential>,
    ) -> Result<OciDigest, MachineRegistryImageResolveError> {
        let mut state =
            self.state
                .lock()
                .map_err(|error| MachineRegistryImageResolveError::ImagePull {
                    message: error.to_string(),
                })?;
        state
            .resolutions
            .push((reference.clone(), credential.cloned()));
        if let Some(message) = &state.resolution_failure {
            return Err(MachineRegistryImageResolveError::ImagePull {
                message: message.clone(),
            });
        }
        Ok(OciDigest::sha256(reference.as_str().as_bytes()))
    }

    async fn create_managed_container(
        &self,
        command: CreateManagedContainer,
    ) -> Result<ContainerId, MachineContainerCreateError> {
        self.state
            .lock()
            .map_err(|error| MachineContainerCreateError::Create {
                message: error.to_string(),
            })?
            .pulls
            .push(command.pull.clone());
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
            .map_err(|error| MachineContainerCreateError::Create {
                message: error.to_string(),
            })?;
        self.replace_snapshot(snapshot);

        Ok(container_id)
    }

    async fn start_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> Result<(), MachineContainerStartError> {
        let snapshot = self.snapshot();
        let Some(observation) = snapshot.container(container_id).cloned() else {
            return Err(missing_container_start_error(container_id));
        };
        // Every container joins the endpoint network at creation (ADR 0023),
        // so a started container always observes an endpoint IP.
        let snapshot = snapshot
            .with_container_replaced(ManagedContainerObservation {
                state: ContainerRuntimeState::Running {
                    ip: Some(std::net::Ipv4Addr::LOCALHOST.into()),
                    health: ployz_core::machine::runtime::ContainerHealth::None,
                    started_at_unix_ms: Some(current_unix_ms()),
                },
                ..observation
            })
            .map_err(|error| MachineContainerStartError::Start {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
        self.replace_snapshot(snapshot);
        Ok(())
    }

    async fn wait_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> Result<i64, MachineContainerWaitError> {
        let snapshot = self.snapshot();
        let Some(observation) = snapshot.container(container_id).cloned() else {
            return Err(MachineContainerWaitError::Wait {
                container_id: container_id.clone(),
                message: "container not found".to_owned(),
            });
        };
        let snapshot = snapshot
            .with_container_replaced(ManagedContainerObservation {
                state: ContainerRuntimeState::Exited,
                ..observation
            })
            .map_err(|error| MachineContainerWaitError::Wait {
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
    ) -> Result<(), MachineContainerRemoveError> {
        let snapshot = self.snapshot();

        let existing = snapshot
            .containers()
            .iter()
            .find(|container| container.container_id == *container_id);
        let Some(existing) = existing else {
            return Ok(());
        };
        if observation_identity(existing) != *expected_identity {
            return Err(MachineContainerRemoveError::Remove {
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
                .map_err(|error| MachineContainerRemoveError::Remove {
                    container_id: container_id.clone(),
                    message: error.to_string(),
                })?;
        self.replace_snapshot(snapshot);
        Ok(())
    }

    async fn remove_volume(
        &self,
        _docker_volume_name: &str,
    ) -> Result<(), MachineVolumeRemoveError> {
        Ok(())
    }

    async fn destroy_provisioned_dataset(
        &self,
        _dataset: &ployz_core::deploy::DatasetName,
    ) -> Result<(), ployz_core::storage::StorageEffectFailure> {
        Ok(())
    }

    async fn stop_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> Result<(), MachineContainerStopError> {
        let snapshot = self.snapshot();

        let Some(existing) = snapshot.container(container_id).cloned() else {
            return Ok(());
        };
        if observation_identity(&existing) != *expected_identity {
            return Err(MachineContainerStopError::Stop {
                container_id: container_id.clone(),
                message: "container identity did not match stop target".to_owned(),
            });
        }

        let snapshot = snapshot
            .with_container_replaced(ManagedContainerObservation {
                state: ContainerRuntimeState::Exited,
                ..existing
            })
            .map_err(|error| MachineContainerStopError::Stop {
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
    ) -> Result<(), MachineContainerRestartError> {
        let snapshot = self.snapshot();

        let Some(existing) = snapshot.container(container_id).cloned() else {
            return Err(MachineContainerRestartError::Restart {
                container_id: container_id.clone(),
                message: "container was not found".to_owned(),
            });
        };
        if observation_identity(&existing) != *expected_identity {
            return Err(MachineContainerRestartError::Restart {
                container_id: container_id.clone(),
                message: "container identity did not match restart target".to_owned(),
            });
        }

        let snapshot = snapshot
            .with_container_replaced(ManagedContainerObservation {
                state: ContainerRuntimeState::Running {
                    ip: Some(std::net::Ipv4Addr::LOCALHOST.into()),
                    health: ployz_core::machine::runtime::ContainerHealth::None,
                    started_at_unix_ms: Some(current_unix_ms()),
                },
                ..existing
            })
            .map_err(|error| MachineContainerRestartError::Restart {
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
        _query: crate::roles::machine::runner::MachineLogQuery,
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
    fn next_container_id(&self) -> Result<ContainerId, MachineContainerCreateError> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| MachineContainerCreateError::Create {
                message: error.to_string(),
            })?;
        state.next_container_id()
    }
}

#[derive(Debug)]
struct ObservingContainerRunnerState {
    next_container_number: u64,
    snapshot: MachineContainerObservationSnapshot,
    endpoint_subnet: Option<MachineEndpointSubnet>,
    resolutions: Vec<(ImageReference, Option<RegistryCredential>)>,
    pulls: Vec<MachineImagePull>,
    resolution_failure: Option<String>,
}

impl ObservingContainerRunnerState {
    fn next_container_id(&mut self) -> Result<ContainerId, MachineContainerCreateError> {
        self.next_container_number += 1;
        ContainerId::try_new(format!("ctr_{}", self.next_container_number)).map_err(|error| {
            MachineContainerCreateError::Create {
                message: error.to_string(),
            }
        })
    }
}

#[derive(Debug, Clone)]
pub struct ReadyWireGuardEbpf {
    public_key: WireGuardPublicKey,
    peers: Arc<Mutex<Vec<WireGuardPeer>>>,
}

impl ReadyWireGuardEbpf {
    #[must_use]
    pub fn for_machine(machine_id: &MachineId) -> Self {
        Self {
            public_key: test_wireguard_public_key(machine_id),
            peers: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[must_use]
pub fn test_wireguard_public_key(machine_id: &MachineId) -> WireGuardPublicKey {
    WireGuardPublicKey::try_new(format!("public-{}", machine_id.as_str()))
        .expect("test public key is valid")
}

impl MachinePloyzNativeMeshPreparer for ReadyWireGuardEbpf {
    async fn read_ployz_native_mesh_status(
        &self,
        _mode: ployz_core::network::NetworkStatusMode,
    ) -> Result<MachineDataplaneStatus, String> {
        let message = ployz_core::operation::FailureMessage::try_new(
            "projection testimony is supplied by the machine projection task",
        )
        .expect("test failure message");
        let peers = self
            .peers
            .lock()
            .expect("ready mesh peer lock is not poisoned")
            .iter()
            .map(|peer| WireGuardPeerStatus {
                public_key: peer.public_key.clone(),
                endpoint_subnet: MachineEndpointSubnet::try_new(&peer.endpoint_subnet).map_or(
                    WireGuardPeerEndpointSubnet::Invalid {
                        value: peer.endpoint_subnet.clone(),
                        message: "invalid test endpoint subnet".to_owned(),
                    },
                    |subnet| WireGuardPeerEndpointSubnet::Valid { subnet },
                ),
                endpoint: Some(peer.active_endpoint),
                handshake: WireGuardHandshakeStatus::Ago { seconds: 1 },
                rtt: WireGuardRttStatus::Unavailable {
                    message: "not measured".to_owned(),
                },
                rx_bytes: 0,
                tx_bytes: 0,
                mtu_probe: WireGuardMtuProbe::NotRequested,
            })
            .collect();
        Ok(MachineDataplaneStatus {
            projection: NativeDataplaneProjectionStatus {
                endpoint_bridge: EndpointBridgeStatus::Missing,
                testimony: DataplaneProjectionTestimony::Unusable {
                    attempted_revisions: None,
                    last_applied_revisions: None,
                    failure: DataplaneProjectionFailure::FetchFailed { message },
                },
            },
            wireguard: WireGuardStatus {
                interface: "ployz-wg0".to_owned(),
                configured_mtu: WireGuardConfiguredMtu::Auto,
                detected_mtu: WireGuardDetectedMtu::Detected { mtu: 1420 },
                interface_mtu: WireGuardInterfaceMtu::Detected { mtu: 1420 },
                peers,
            },
            ebpf_attachment: EbpfAttachmentStatus::Attached,
        })
    }

    async fn read_wireguard_public_key(
        &self,
    ) -> Result<WireGuardPublicKey, WireGuardEbpfPrepareError> {
        Ok(self.public_key.clone())
    }

    async fn prepare_ployz_native_mesh(
        &self,
        _endpoint_routes: &[ployz_core::network::WireGuardEbpfEndpointRoute],
        peers: &[WireGuardPeer],
    ) -> Result<PloyzNativeMeshReady, WireGuardEbpfPrepareError> {
        *self
            .peers
            .lock()
            .expect("ready mesh peer lock is not poisoned") = peers.to_vec();
        Ok(PloyzNativeMeshReady {
            wireguard: WireGuardReady {
                public_key: self.public_key.clone(),
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
        endpoint_routes: &[ployz_core::network::WireGuardEbpfEndpointRoute],
        peers: &[ployz_core::network::WireGuardPeer],
    ) -> Result<WireGuardReady, WireGuardEbpfPrepareError> {
        self.prepare_ployz_native_mesh(endpoint_routes, peers)
            .await
            .map(|ready| ready.wireguard)
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
        ContainerRuntimeState::Running {
            ip,
            health,
            started_at_unix_ms,
        } => ExistingManagedContainerState::Running {
            ip: *ip,
            health: *health,
            started_at_unix_ms: *started_at_unix_ms,
        },
        ContainerRuntimeState::Exited => ExistingManagedContainerState::StartableStopped,
    }
}

fn current_unix_ms() -> u64 {
    let Ok(elapsed) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return 0;
    };
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

fn empty_snapshot(machine_id: &MachineId) -> MachineContainerObservationSnapshot {
    MachineContainerObservationSnapshot::try_new(machine_id.clone(), Vec::new())
        .expect("empty machine snapshot is valid")
}

fn missing_container_start_error(container_id: &ContainerId) -> MachineContainerStartError {
    MachineContainerStartError::Start {
        container_id: container_id.clone(),
        message: "container is missing from observed runner state".to_owned(),
    }
}
