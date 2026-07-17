use std::future::Future;

use ployz_core::deploy::{
    ContainerRuntimeSpec, DatasetName, ImageReference, RegistryCredential, VolumeName,
};
use ployz_core::ids::ContainerId;
use ployz_core::image::OciDigest;
use ployz_core::intent::VolumePinState;
use ployz_core::machine::runtime::ContainerHealth;
use ployz_core::machine::{VolumeEnsureFailure, VolumeUsageFacts};
use std::net::IpAddr;

use crate::roles::machine::protocol::MachineImagePull;
use ployz_core::machine::runtime::{ManagedContainerHealthStatus, ManagedContainerIdentity};
use ployz_core::network::{EndpointBridgeStatus, MachineEndpointSubnet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingManagedContainer {
    pub container_id: ContainerId,
    pub identity: ManagedContainerIdentity,
    pub state: ExistingManagedContainerState,
    pub health_status: Option<ManagedContainerHealthStatus>,
    pub resolved_image_identity: Option<String>,
    pub created_at_unix_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExistingManagedContainerState {
    Running {
        ip: Option<IpAddr>,
        health: ContainerHealth,
        started_at_unix_ms: Option<u64>,
    },
    StartableStopped,
    NotStartable {
        description: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateManagedContainer {
    pub pull: MachineImagePull,
    pub runtime: ContainerRuntimeSpec,
    pub provisioned_volumes: Vec<VolumeName>,
    pub identity: ManagedContainerIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineContainerRunnerError {
    ListExisting {
        message: String,
    },
    EnsureEndpointNetwork {
        message: String,
    },
    EndpointNetworkSubnetMismatch {
        expected: MachineEndpointSubnet,
        observed: MachineEndpointSubnet,
    },
    Create {
        message: String,
    },
    ImagePull {
        message: String,
    },
    Start {
        container_id: ContainerId,
        message: String,
    },
    Wait {
        container_id: ContainerId,
        message: String,
    },
    Stop {
        container_id: ContainerId,
        message: String,
    },
    Restart {
        container_id: ContainerId,
        message: String,
    },
    Remove {
        container_id: ContainerId,
        message: String,
    },
    RemoveVolume {
        docker_volume_name: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineLogReaderError {
    NotFound {
        container_id: ContainerId,
    },
    ReadFailed {
        container_id: ContainerId,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineLogTail {
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineLogQuery {
    pub tail_lines: Option<u16>,
    pub since_unix_seconds: Option<u64>,
    pub timestamps: MachineLogTimestamps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineLogTimestamps {
    Include,
    Omit,
}

pub trait MachineContainerRunner {
    fn ensure_volume(
        &self,
        volume: &VolumePinState,
    ) -> impl Future<Output = Result<(), VolumeEnsureFailure>> + Send;

    fn existing_managed_containers(
        &self,
    ) -> impl Future<Output = Result<Vec<ExistingManagedContainer>, MachineContainerRunnerError>> + Send;

    fn ensure_endpoint_network(
        &self,
    ) -> impl Future<Output = Result<(), MachineContainerRunnerError>> + Send;

    fn ensure_projection_endpoint_network(
        &self,
        expected_subnet: &MachineEndpointSubnet,
    ) -> impl Future<Output = Result<(), MachineContainerRunnerError>> + Send;

    fn read_endpoint_network_status(&self) -> impl Future<Output = EndpointBridgeStatus> + Send;

    fn resolve_registry_image(
        &self,
        reference: &ImageReference,
        credential: Option<&RegistryCredential>,
    ) -> impl Future<Output = Result<OciDigest, MachineContainerRunnerError>> + Send;

    fn create_managed_container(
        &self,
        command: CreateManagedContainer,
    ) -> impl Future<Output = Result<ContainerId, MachineContainerRunnerError>> + Send;

    fn start_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> impl Future<Output = Result<(), MachineContainerRunnerError>> + Send;

    fn wait_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> impl Future<Output = Result<i64, MachineContainerRunnerError>> + Send;

    fn stop_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> impl Future<Output = Result<(), MachineContainerRunnerError>> + Send;

    fn restart_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> impl Future<Output = Result<(), MachineContainerRunnerError>> + Send;

    fn remove_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> impl Future<Output = Result<(), MachineContainerRunnerError>> + Send;

    fn remove_volume(
        &self,
        docker_volume_name: &str,
    ) -> impl Future<Output = Result<(), MachineContainerRunnerError>> + Send;

    fn destroy_provisioned_dataset(
        &self,
        dataset: &DatasetName,
    ) -> impl Future<Output = Result<(), ployz_core::storage::StorageEffectFailure>> + Send;
}

/// Fresh, machine-owned volume testimony. Implementations must return `None`
/// whenever either usage or latest-write evidence cannot be gathered fully.
pub trait MachineVolumeUsageReader {
    fn read_volume_usage(
        &self,
        volume: &VolumePinState,
    ) -> impl Future<Output = Option<VolumeUsageFacts>> + Send;
}

pub trait MachineImageRemovalRunner {
    fn remove_image(
        &self,
        image_identity: &OciDigest,
    ) -> impl Future<Output = Result<ployz_core::image::ImageRemoveOutcome, String>> + Send;
}

pub trait MachineLogReader {
    fn tail_container_logs(
        &self,
        container_id: &ContainerId,
        query: MachineLogQuery,
    ) -> impl Future<Output = Result<MachineLogTail, MachineLogReaderError>> + Send;
}
