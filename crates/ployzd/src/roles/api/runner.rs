use std::collections::BTreeSet;
use std::future::Future;

use ployz_core::corrosion::{CorrosionNamespaceName, V2ManagedContainerIdentity};
use ployz_core::deploy::{ContainerRuntimeSpec, ImageReference, RegistryCredential, VolumeName};
use ployz_core::ids::ContainerId;
use ployz_core::image::OciDigest;
use ployz_core::intent::VolumePinState;
use ployz_core::machine::runtime::ContainerHealth;
use ployz_core::machine::{VolumeEnsureFailure, VolumeUsageFacts};
use std::net::IpAddr;

use ployz_core::machine::runtime::{ManagedContainerHealthStatus, ManagedContainerIdentity};
use ployz_core::network::{EndpointBridgeStatus, MachineEndpointSubnet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineContainerStopOutcome {
    StoppedRunning,
    AlreadyStopped,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingManagedContainer {
    pub container_id: ContainerId,
    pub identity: ManagedContainerIdentity,
    pub state: ExistingManagedContainerState,
    pub health_status: Option<ManagedContainerHealthStatus>,
    pub resolved_image_identity: Option<String>,
    pub created_at_unix_seconds: Option<i64>,
    pub named_volume_names: BTreeSet<VolumeName>,
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
    pub image: ImageReference,
    pub runtime: ContainerRuntimeSpec,
    pub provisioned_volumes: Vec<VolumeName>,
    pub identity: ManagedContainerIdentity,
}

/// A Corrosion-owned container recovered from its row-id-only Docker labels.
///
/// This is intentionally separate from [`ExistingManagedContainer`]: the
/// incumbent revision-and-step identity is not part of the v2 runtime model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingV2ManagedContainer {
    pub container_id: ContainerId,
    pub identity: V2ManagedContainerIdentity,
    pub state: ExistingManagedContainerState,
    pub health_status: Option<ManagedContainerHealthStatus>,
    pub resolved_image_identity: Option<String>,
    pub created_at_unix_seconds: Option<i64>,
}

/// Complete Docker input for one Corrosion-owned service container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateV2ManagedContainer {
    pub image: ImageReference,
    pub runtime: ContainerRuntimeSpec,
    pub namespace_name: CorrosionNamespaceName,
    pub identity: V2ManagedContainerIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineContainerListError {
    ListExisting { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineEndpointNetworkError {
    EnsureEndpointNetwork {
        message: String,
    },
    EndpointNetworkSubnetMismatch {
        expected: MachineEndpointSubnet,
        observed: MachineEndpointSubnet,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineEndpointNetworkConvergenceOutcome {
    Unchanged,
    Created,
    Recreated {
        reattached_containers: usize,
        restarted_containers: usize,
    },
    RecoveredAttachments {
        reattached_containers: usize,
        restarted_containers: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineEndpointNetworkConvergenceError {
    #[error("could not inspect the local Docker endpoint network: {message}")]
    Inspect { message: String },
    #[error("the local Docker endpoint network is not owned by Ployz")]
    UnmanagedNetwork,
    #[error("could not list local Ployz-managed containers: {message}")]
    ListManagedContainers { message: String },
    #[error("Docker returned a Ployz-managed container without an id")]
    ManagedContainerMissingId,
    #[error("the Ployz endpoint network has unmanaged attached containers: {container_ids:?}")]
    UnmanagedAttachments { container_ids: Vec<String> },
    #[error(
        "could not disconnect container {container_id} from the old Ployz endpoint network: {message}"
    )]
    Disconnect {
        container_id: String,
        message: String,
    },
    #[error("could not remove the old Ployz endpoint network: {message}")]
    Remove { message: String },
    #[error("could not create the Ployz endpoint network: {message}")]
    Create { message: String },
    #[error("could not restart container {container_id} after endpoint-network repair: {message}")]
    Restart {
        container_id: String,
        message: String,
    },
    #[error(
        "could not attach container {container_id} to the repaired Ployz endpoint network: {message}"
    )]
    Connect {
        container_id: String,
        message: String,
    },
    #[error("could not verify the repaired Ployz endpoint network: {message}")]
    Verify { message: String },
    #[error(
        "the repaired Ployz endpoint network is still missing managed containers: {container_ids:?}"
    )]
    MissingAttachments { container_ids: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineRegistryImageResolveError {
    ImagePull { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum V2MachineImagePullError {
    #[error("the image reference is not digest-pinned")]
    ReferenceNotDigestPinned,
    #[error("the local container runtime is unavailable")]
    RuntimeUnavailable,
    #[error("the local container runtime could not pull the image")]
    PullFailed,
    #[error("the local image does not match the requested digest {expected}")]
    DigestMismatch { expected: OciDigest },
    #[error("the bounded image pull timed out")]
    TimedOut,
    #[error("the image pull was cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineContainerCreateError {
    Create {
        message: String,
    },
    EnsureEndpointNetwork {
        message: String,
    },
    EndpointNetworkSubnetMismatch {
        expected: MachineEndpointSubnet,
        observed: MachineEndpointSubnet,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineContainerStartError {
    Start {
        container_id: ContainerId,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineContainerWaitError {
    Wait {
        container_id: ContainerId,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineContainerStopError {
    Stop {
        container_id: ContainerId,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineContainerRestartError {
    ListExisting {
        message: String,
    },
    Restart {
        container_id: ContainerId,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineContainerRemoveError {
    ListExisting {
        message: String,
    },
    Remove {
        container_id: ContainerId,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineVolumeRemoveError {
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

/// Runtime-neutral origin for one complete container log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeLogStream {
    Stdout,
    Stderr,
}

/// One complete line read from a machine-local container runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLogLine {
    pub stream: RuntimeLogStream,
    pub line: String,
}

/// One bounded log read. Truncation is machine-local evidence, not continuity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLogBatch {
    pub lines: Vec<RuntimeLogLine>,
    pub truncated: bool,
}

/// One owned, bounded follow attachment to the machine-local runtime.
#[derive(Debug)]
pub struct RuntimeLogFollow {
    receiver: tokio::sync::mpsc::Receiver<Result<RuntimeLogLine, V2MachineLogReadError>>,
}

impl RuntimeLogFollow {
    pub(crate) fn new(
        receiver: tokio::sync::mpsc::Receiver<Result<RuntimeLogLine, V2MachineLogReadError>>,
    ) -> Self {
        Self { receiver }
    }

    /// Returns a complete line immediately, `None` on actual runtime EOF, or
    /// the typed reason the attachment ended abnormally.
    pub async fn next_line(&mut self) -> Result<Option<RuntimeLogLine>, V2MachineLogReadError> {
        match self.receiver.recv().await {
            Some(Ok(line)) => Ok(Some(line)),
            Some(Err(error)) => Err(error),
            None => Ok(None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum V2MachineLogReadError {
    #[error("managed container {container_id:?} was not found")]
    NotFound { container_id: ContainerId },
    #[error("the local container runtime is unavailable")]
    RuntimeUnavailable,
    #[error("the local container runtime could not read logs")]
    ReadFailed,
    #[error("the bounded container log read timed out")]
    TimedOut,
    #[error("the container log read was cancelled")]
    Cancelled,
}

pub trait MachineContainerRunner {
    fn ensure_volume(
        &self,
        volume: &VolumePinState,
    ) -> impl Future<Output = Result<(), VolumeEnsureFailure>> + Send;

    fn existing_managed_containers(
        &self,
    ) -> impl Future<Output = Result<Vec<ExistingManagedContainer>, MachineContainerListError>> + Send;

    fn ensure_endpoint_network(
        &self,
    ) -> impl Future<Output = Result<(), MachineEndpointNetworkError>> + Send;

    fn ensure_projection_endpoint_network(
        &self,
        expected_subnet: &MachineEndpointSubnet,
    ) -> impl Future<Output = Result<(), MachineEndpointNetworkError>> + Send;

    fn read_endpoint_network_status(&self) -> impl Future<Output = EndpointBridgeStatus> + Send;

    fn resolve_registry_image(
        &self,
        reference: &ImageReference,
        credential: Option<&RegistryCredential>,
    ) -> impl Future<Output = Result<OciDigest, MachineRegistryImageResolveError>> + Send;

    fn create_managed_container(
        &self,
        command: CreateManagedContainer,
    ) -> impl Future<Output = Result<ContainerId, MachineContainerCreateError>> + Send;

    fn start_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> impl Future<Output = Result<(), MachineContainerStartError>> + Send;

    fn wait_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> impl Future<Output = Result<i64, MachineContainerWaitError>> + Send;

    fn stop_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> impl Future<Output = Result<MachineContainerStopOutcome, MachineContainerStopError>> + Send;

    fn restart_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> impl Future<Output = Result<(), MachineContainerRestartError>> + Send;

    fn remove_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> impl Future<Output = Result<(), MachineContainerRemoveError>> + Send;

    fn remove_volume(
        &self,
        docker_volume_name: &str,
    ) -> impl Future<Output = Result<(), MachineVolumeRemoveError>> + Send;
}

/// Docker effects and recovery testimony owned by the v2 operation driver.
///
/// Start is repeated here rather than routing v2 callers through the frozen
/// incumbent trait. A container id remains sufficient for bounded log reads,
/// which use the separate [`V2MachineLogReader`] seam below.
pub trait V2MachineContainerRunner {
    fn existing_v2_managed_containers(
        &self,
    ) -> impl Future<Output = Result<Vec<ExistingV2ManagedContainer>, MachineContainerListError>> + Send;

    fn create_v2_managed_container(
        &self,
        command: CreateV2ManagedContainer,
    ) -> impl Future<Output = Result<ContainerId, MachineContainerCreateError>> + Send;

    fn start_v2_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> impl Future<Output = Result<(), MachineContainerStartError>> + Send;
}

/// Exact-pinned image acquisition owned by the v2 operation driver.
pub trait V2MachineImageRunner {
    fn pull_v2_registry_image(
        &self,
        reference: &ImageReference,
        credential: Option<&RegistryCredential>,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> impl Future<Output = Result<(), V2MachineImagePullError>> + Send;
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

/// Bounded v2 reads that preserve the runtime's stdout/stderr distinction.
pub trait V2MachineLogReader {
    fn tail_v2_container_logs(
        &self,
        container_id: &ContainerId,
        tail_lines: u16,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> impl Future<Output = Result<RuntimeLogBatch, V2MachineLogReadError>> + Send;

    fn open_v2_container_log_follow(
        &self,
        container_id: &ContainerId,
        tail_lines: u16,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> impl Future<Output = Result<RuntimeLogFollow, V2MachineLogReadError>> + Send;
}
