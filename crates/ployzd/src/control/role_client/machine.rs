//! Request-side NATS adapters for machine-local services.

use crate::roles::machine::MachineRuntimeUnavailableReason;
use crate::roles::machine::protocol::{
    MachineBuildCachePruneDomainError, MachineBuildCachePruneRpcOk,
    MachineBuildCachePruneRpcRequest, MachineBuildCapability, MachineContainerInspectDomainError,
    MachineContainerInspectRpcOk, MachineContainerInspectRpcRequest,
    MachineContainerRemoveDomainError, MachineContainerRemoveRpcRequest,
    MachineContainerResolveImageDomainError, MachineContainerResolveImageRpcOk,
    MachineContainerResolveImageRpcRequest, MachineContainerRestartDomainError,
    MachineContainerRestartRpcRequest, MachineContainerRpcOk, MachineContainerRunDomainError,
    MachineContainerRunHookDomainError, MachineContainerRunHookRpcOk,
    MachineContainerRunHookRpcRequest, MachineContainerRunRpcOk, MachineContainerRunRpcRequest,
    MachineContainerStopDomainError, MachineContainerStopRpcRequest,
    MachineDataplaneStatusDomainError, MachineDataplaneStatusRpcOk,
    MachineDataplaneStatusRpcRequest, MachineFactsGetDomainError, MachineFactsGetRpcOk,
    MachineFactsGetRpcRequest, MachineFactsRefreshDomainError, MachineFactsRefreshRpcOk,
    MachineFactsRefreshRpcRequest, MachineLogsTailDomainError, MachineLogsTailResult,
    MachineLogsTailRpcOk, MachineLogsTailRpcRequest, MachineRpcResponder, MachineRpcResponse,
    MachineStoragePrepareDomainError, MachineStoragePrepareReportRpcOk,
    MachineStoragePrepareReportRpcRequest, MachineStoragePrepareRpcOk,
    MachineStoragePrepareRpcRequest, MachineSubstrateReportRpcOk, MachineSubstrateReportRpcRequest,
    MachineSubstrateUpdateDomainError, MachineSubstrateUpdateRpcOk,
    MachineSubstrateUpdateRpcRequest, MachineVolumeEnsureRpcOk, MachineVolumeEnsureRpcRequest,
    MachineVolumeRemoveDomainError, MachineVolumeRemoveRpcOk, MachineVolumeRemoveRpcRequest,
};
use futures_util::{StreamExt, stream};
use ployz_core::deploy::VolumeName;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::image::{
    ImageEnsureOk, ImageEnsureRequest, ImageRemoveOk, ImageRemoveRequest, ImageRpcDomainError,
};
use ployz_core::intent::{ProvisionedVolumePinState, VolumePinState};
use ployz_core::machine::MachineLifecycle;
use ployz_core::machine::runtime::{MachineContainerObservationSnapshot, MachineFactsSnapshot};
use ployz_core::network::{MachineDataplaneStatus, NetworkStatusMode};
use ployz_core::operation::{MachineSubstrateVersions, MachineUpdateFailure};
use ployz_nats::service_protocol::{NatsServiceError, NatsServiceErrorCode};
use ployz_nats::service_runtime::{
    NatsJsonServiceRequestError, NatsServiceRequestFailure, request_json,
};
use ployz_nats::subjects::{MachineServiceEndpoint, machine_service};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::time::Duration;

pub const DEFAULT_MACHINE_RPC_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const MAX_CONCURRENT_MACHINE_READS: usize = 16;

mod volume_testimony;

pub use volume_testimony::{MachineVolumeTestimonyReadError, NatsMachineVolumeTestimonyReader};

#[derive(Debug, Clone)]
pub struct NatsMachineContainerRuntime {
    client: async_nats::Client,
    request_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct NatsMachineFactsReader {
    pub(super) client: async_nats::Client,
    pub(super) request_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct NatsMachineLogsTailer {
    client: async_nats::Client,
    request_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct NatsMachineSubstrateUpdater {
    client: async_nats::Client,
    request_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineImageEnsureError {
    Domain {
        machine_id: MachineId,
        error: ImageRpcDomainError,
    },
    Unavailable {
        machine_id: MachineId,
        reason: MachineRuntimeUnavailableReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineImageRemoveError {
    Domain {
        machine_id: MachineId,
        error: ployz_core::image::ImageRemoveDomainError,
    },
    Unavailable {
        machine_id: MachineId,
        reason: MachineRuntimeUnavailableReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineImageResolveError {
    Rejected {
        machine_id: MachineId,
        message: ployz_core::operation::FailureMessage,
    },
    Unavailable {
        machine_id: MachineId,
        reason: MachineRuntimeUnavailableReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineSubstrateUpdateError {
    Unavailable {
        machine_id: MachineId,
        reason: MachineRuntimeUnavailableReason,
    },
    UpdateFailed {
        machine_id: MachineId,
        message: ployz_core::operation::FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineStoragePrepareError {
    Unavailable {
        machine_id: MachineId,
        reason: MachineRuntimeUnavailableReason,
    },
    PreparationFailed {
        machine_id: MachineId,
        failure: ployz_core::storage::StorageEffectFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineLogsTailError {
    NotFound {
        machine_id: MachineId,
        container_id: ployz_core::ids::ContainerId,
    },
    ReadFailed {
        machine_id: MachineId,
        container_id: ployz_core::ids::ContainerId,
        message: ployz_core::operation::FailureMessage,
    },
    Unavailable {
        machine_id: MachineId,
        reason: MachineRuntimeUnavailableReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineFactsReadError {
    #[error("machine {} rejected facts gather: {}", machine_id.as_str(), message.as_str())]
    GatherFailed {
        machine_id: MachineId,
        message: ployz_core::operation::FailureMessage,
    },
    #[error(
        "machine {} facts unavailable: {}",
        machine_id.as_str(),
        reason.failure_message().as_str()
    )]
    Unavailable {
        machine_id: MachineId,
        reason: MachineRuntimeUnavailableReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineFactsRefreshError {
    #[error("machine {} facts refresh failed: {}", machine_id.as_str(), message.as_str())]
    RefreshFailed {
        machine_id: MachineId,
        message: ployz_core::operation::FailureMessage,
    },
    #[error(
        "machine {} facts refresh unavailable: {}",
        machine_id.as_str(),
        reason.failure_message().as_str()
    )]
    Unavailable {
        machine_id: MachineId,
        reason: MachineRuntimeUnavailableReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineContainerInspectError {
    InspectFailed {
        machine_id: MachineId,
        container_id: ployz_core::ids::ContainerId,
        message: ployz_core::operation::FailureMessage,
    },
    Unavailable {
        machine_id: MachineId,
        reason: MachineRuntimeUnavailableReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineVolumeRemoveError {
    #[error("machine {} volume remove unavailable: {}", machine_id.as_str(), message.as_str())]
    Unavailable {
        machine_id: MachineId,
        message: ployz_core::operation::FailureMessage,
    },
    #[error("machine {} rejected volume removal", machine_id.as_str())]
    Domain {
        machine_id: MachineId,
        error: MachineVolumeRemoveDomainError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineVolumeEnsureError {
    Unavailable {
        machine_id: MachineId,
        volume_name: VolumeName,
        reason: MachineRuntimeUnavailableReason,
    },
    Domain {
        machine_id: MachineId,
        volume_name: VolumeName,
        failure: ployz_core::machine::VolumeEnsureFailure,
    },
}

/// Outcome of one machine RPC round trip: either the machine answered with a typed
/// domain error, or the call never produced a usable answer.
#[derive(Debug)]
pub(crate) enum MachineCallError<E> {
    Unavailable(MachineRuntimeUnavailableReason),
    Domain(E),
}

/// One machine RPC round trip: encode the request, map transport failures, and
/// reject answers from the wrong machine — exactly once for every endpoint.
pub(crate) async fn call_machine<T, E>(
    client: &async_nats::Client,
    request_timeout: Duration,
    machine_id: &MachineId,
    endpoint: MachineServiceEndpoint,
    request: &impl Serialize,
) -> Result<T, MachineCallError<E>>
where
    T: DeserializeOwned + MachineRpcResponder,
    E: DeserializeOwned,
{
    let subject = machine_service(machine_id, endpoint);
    let response =
        request_json::<_, MachineRpcResponse<T, E>>(client, subject, request, request_timeout)
            .await
            .map_err(|error| MachineCallError::Unavailable(unavailable_reason(error)))?;

    match response {
        MachineRpcResponse::Ok(value) => {
            match wrong_response_machine(machine_id, value.responder_machine_id().clone()) {
                Some(reason) => Err(MachineCallError::Unavailable(reason)),
                None => Ok(value),
            }
        }
        MachineRpcResponse::DomainError {
            machine_id: actual_machine_id,
            error,
        } => match wrong_response_machine(machine_id, actual_machine_id) {
            Some(reason) => Err(MachineCallError::Unavailable(reason)),
            None => Err(MachineCallError::Domain(error)),
        },
    }
}

impl NatsMachineLogsTailer {
    #[must_use]
    pub fn new(client: async_nats::Client) -> Self {
        Self {
            client,
            request_timeout: DEFAULT_MACHINE_RPC_TIMEOUT,
        }
    }

    pub async fn tail_logs(
        &self,
        machine_id: &MachineId,
        request: MachineLogsTailRpcRequest,
    ) -> Result<MachineLogsTailResult, MachineLogsTailError> {
        call_machine::<MachineLogsTailRpcOk, MachineLogsTailDomainError>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::LogsTail,
            &request,
        )
        .await
        .map(|ok| ok.value)
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => MachineLogsTailError::Unavailable {
                machine_id: machine_id.clone(),
                reason,
            },
            MachineCallError::Domain(error) => error.into_runtime_error(machine_id.clone()),
        })
    }
}

impl NatsMachineFactsReader {
    #[must_use]
    pub fn new(client: async_nats::Client) -> Self {
        Self {
            client,
            request_timeout: DEFAULT_MACHINE_RPC_TIMEOUT,
        }
    }

    #[must_use]
    pub const fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
    }

    pub async fn machine_facts(
        &self,
        machine_id: &MachineId,
    ) -> Result<MachineFactsSnapshot, MachineFactsReadError> {
        self.machine_facts_response(machine_id)
            .await
            .map(|ok| ok.facts)
    }

    pub(crate) async fn machine_facts_response(
        &self,
        machine_id: &MachineId,
    ) -> Result<MachineFactsGetRpcOk, MachineFactsReadError> {
        call_machine::<MachineFactsGetRpcOk, MachineFactsGetDomainError>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::FactsGet,
            &MachineFactsGetRpcRequest {},
        )
        .await
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => MachineFactsReadError::Unavailable {
                machine_id: machine_id.clone(),
                reason,
            },
            MachineCallError::Domain(error) => error.into_runtime_error(machine_id.clone()),
        })
    }

    pub async fn refresh_machine_facts(
        &self,
        machine_id: &MachineId,
    ) -> Result<
        ployz_core::machine::runtime::MachineFactsRefreshConfirmation,
        MachineFactsRefreshError,
    > {
        call_machine::<MachineFactsRefreshRpcOk, MachineFactsRefreshDomainError>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::FactsRefresh,
            &MachineFactsRefreshRpcRequest {},
        )
        .await
        .map(|ok| ok.refresh)
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => MachineFactsRefreshError::Unavailable {
                machine_id: machine_id.clone(),
                reason,
            },
            MachineCallError::Domain(MachineFactsRefreshDomainError::RefreshFailed { message }) => {
                MachineFactsRefreshError::RefreshFailed {
                    machine_id: machine_id.clone(),
                    message,
                }
            }
        })
    }
}

pub(crate) struct MachinePlacementFacts {
    pub machine_id: MachineId,
    pub lifecycle: MachineLifecycle,
    pub answer: Option<MachinePlacementFactsAnswer>,
}

pub(crate) struct MachinePlacementFactsAnswer {
    pub containers: MachineContainerObservationSnapshot,
    pub platform: ployz_core::image::OciPlatform,
    pub endpoints: Option<ployz_core::machine::MachineEndpointObservation>,
    pub storage: Option<ployz_core::machine::StorageCapability>,
    pub build: MachineBuildCapability,
    pub clock: MachineClockTestimony,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MachineClockTestimony {
    pub control_request_started_at_unix_ms: u64,
    pub machine_observed_at_unix_ms: u64,
}

pub(crate) async fn read_machine_placement_facts(
    facts_reader: &NatsMachineFactsReader,
    machine_lifecycles: impl IntoIterator<Item = (MachineId, MachineLifecycle)>,
) -> Vec<MachinePlacementFacts> {
    let mut reads = machine_placement_fact_reads(
        facts_reader,
        machine_lifecycles.into_iter().collect::<Vec<_>>(),
        MAX_CONCURRENT_MACHINE_READS,
    );

    let mut facts = Vec::new();
    while let Some(machine) = reads.next().await {
        facts.push(machine);
    }
    facts.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
    facts
}

pub(crate) async fn read_machine_placement_facts_with_gather_timeout(
    facts_reader: &NatsMachineFactsReader,
    machine_lifecycles: impl IntoIterator<Item = (MachineId, MachineLifecycle)>,
    gather_timeout: Duration,
) -> Vec<MachinePlacementFacts> {
    let candidates = machine_lifecycles.into_iter().collect::<Vec<_>>();
    let reads =
        machine_placement_fact_reads(facts_reader, candidates.clone(), candidates.len().max(1));
    collect_machine_placement_facts_until(candidates, reads, gather_timeout).await
}

async fn collect_machine_placement_facts_until(
    candidates: Vec<(MachineId, MachineLifecycle)>,
    reads: impl futures_util::Stream<Item = MachinePlacementFacts>,
    gather_timeout: Duration,
) -> Vec<MachinePlacementFacts> {
    let mut unfinished = candidates.into_iter().collect::<BTreeMap<_, _>>();
    futures_util::pin_mut!(reads);
    let mut facts = Vec::new();
    let _ = tokio::time::timeout(gather_timeout, async {
        while let Some(machine) = reads.next().await {
            unfinished.remove(&machine.machine_id);
            facts.push(machine);
        }
    })
    .await;
    facts.extend(
        unfinished
            .into_iter()
            .map(|(machine_id, lifecycle)| MachinePlacementFacts {
                machine_id,
                lifecycle,
                answer: None,
            }),
    );
    facts.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
    facts
}

fn machine_placement_fact_reads(
    facts_reader: &NatsMachineFactsReader,
    machine_lifecycles: Vec<(MachineId, MachineLifecycle)>,
    concurrency: usize,
) -> impl futures_util::Stream<Item = MachinePlacementFacts> + '_ {
    stream::iter(machine_lifecycles)
        .map(move |(machine_id, lifecycle)| async move {
            let control_request_started_at_unix_ms = current_unix_millis();
            let answer = facts_reader
                .machine_facts_response(&machine_id)
                .await
                .ok()
                .map(|response| placement_answer(response, control_request_started_at_unix_ms));
            MachinePlacementFacts {
                machine_id,
                lifecycle,
                answer,
            }
        })
        .buffer_unordered(concurrency)
}

fn placement_answer(
    response: MachineFactsGetRpcOk,
    control_request_started_at_unix_ms: u64,
) -> MachinePlacementFactsAnswer {
    MachinePlacementFactsAnswer {
        containers: response.facts.containers().clone(),
        platform: response.facts.platform().clone(),
        endpoints: response.facts.endpoints().cloned(),
        storage: response.facts.storage().cloned(),
        build: response.build,
        clock: MachineClockTestimony {
            control_request_started_at_unix_ms,
            machine_observed_at_unix_ms: response.facts.observed_at_unix_ms(),
        },
    }
}

fn current_unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

pub(crate) async fn read_available_machine_facts(
    facts_reader: &NatsMachineFactsReader,
    machine_ids: impl IntoIterator<Item = MachineId>,
) -> Vec<MachineFactsSnapshot> {
    let mut reads = stream::iter(machine_ids)
        .map(|machine_id| async move { facts_reader.machine_facts(&machine_id).await.ok() })
        .buffer_unordered(MAX_CONCURRENT_MACHINE_READS);

    let mut facts = Vec::new();
    // Drain every read: an unavailable machine resolves to None and must be
    // skipped, never end the gather, or one down machine would cancel the
    // still-pending reads for healthy machines completing after it.
    while let Some(snapshot) = reads.next().await {
        if let Some(snapshot) = snapshot {
            facts.push(snapshot);
        }
    }
    facts.sort_by(|left, right| left.machine_id().cmp(right.machine_id()));
    facts
}

pub(crate) async fn read_available_machine_facts_by_id(
    facts_reader: &NatsMachineFactsReader,
    machine_ids: impl IntoIterator<Item = MachineId>,
) -> BTreeMap<MachineId, MachineFactsSnapshot> {
    read_available_machine_facts(facts_reader, machine_ids)
        .await
        .into_iter()
        .map(|facts| (facts.machine_id().clone(), facts))
        .collect()
}

impl NatsMachineSubstrateUpdater {
    #[must_use]
    pub fn new(client: async_nats::Client) -> Self {
        Self {
            client,
            request_timeout: DEFAULT_MACHINE_RPC_TIMEOUT,
        }
    }

    pub async fn update_substrate(
        &self,
        machine_id: &MachineId,
        request: MachineSubstrateUpdateRpcRequest,
    ) -> Result<(), MachineSubstrateUpdateError> {
        call_machine::<MachineSubstrateUpdateRpcOk, MachineSubstrateUpdateDomainError>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::SubstrateUpdate,
            &request,
        )
        .await
        .map(|_| ())
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => MachineSubstrateUpdateError::Unavailable {
                machine_id: machine_id.clone(),
                reason,
            },
            MachineCallError::Domain(error) => error.into_runtime_error(machine_id.clone()),
        })
    }

    pub async fn report_substrate_versions(
        &self,
        machine_id: &MachineId,
        operation_id: &OperationId,
    ) -> Result<MachineSubstrateVersions, MachineSubstrateUpdateError> {
        call_machine::<MachineSubstrateReportRpcOk, MachineSubstrateUpdateDomainError>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::SubstrateReport,
            &MachineSubstrateReportRpcRequest {
                operation_id: operation_id.clone(),
            },
        )
        .await
        .map(|ok| ok.reported)
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => MachineSubstrateUpdateError::Unavailable {
                machine_id: machine_id.clone(),
                reason,
            },
            MachineCallError::Domain(error) => error.into_runtime_error(machine_id.clone()),
        })
    }

    pub async fn prepare_storage(
        &self,
        machine_id: &MachineId,
        request: MachineStoragePrepareRpcRequest,
    ) -> Result<ployz_core::deploy::ZfsPoolName, MachineStoragePrepareError> {
        call_machine::<MachineStoragePrepareRpcOk, MachineStoragePrepareDomainError>(
            &self.client,
            ployz_core::storage::MACHINE_STORAGE_PREPARE_RPC_TIMEOUT,
            machine_id,
            MachineServiceEndpoint::StoragePrepare,
            &request,
        )
        .await
        .map(|response| response.pool)
        .map_err(|error| storage_prepare_error(machine_id, error))
    }

    pub async fn prune_build_cache(
        &self,
        machine_id: &MachineId,
        operation_id: &OperationId,
    ) -> Result<ployz_core::operation::BuildCachePruneEvidence, MachineBuildCachePruneError> {
        call_machine::<MachineBuildCachePruneRpcOk, MachineBuildCachePruneDomainError>(
            &self.client,
            ployz_core::build::BUILD_START_ENDPOINT_TIMEOUT,
            machine_id,
            MachineServiceEndpoint::BuildCachePrune,
            &MachineBuildCachePruneRpcRequest {
                operation_id: operation_id.clone(),
            },
        )
        .await
        .map(|response| response.evidence)
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => MachineBuildCachePruneError::Unavailable {
                machine_id: machine_id.clone(),
                reason,
            },
            MachineCallError::Domain(MachineBuildCachePruneDomainError::PruneFailed {
                message,
            }) => MachineBuildCachePruneError::PruneFailed {
                machine_id: machine_id.clone(),
                message,
            },
        })
    }

    pub async fn report_storage_prepare(
        &self,
        machine_id: &MachineId,
        operation_id: &OperationId,
    ) -> Result<Option<ployz_core::deploy::ZfsPoolName>, MachineStoragePrepareError> {
        call_machine::<MachineStoragePrepareReportRpcOk, MachineStoragePrepareDomainError>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::StoragePrepareReport,
            &MachineStoragePrepareReportRpcRequest {
                operation_id: operation_id.clone(),
            },
        )
        .await
        .map(|response| response.pool)
        .map_err(|error| storage_prepare_error(machine_id, error))
    }
}

fn storage_prepare_error(
    machine_id: &MachineId,
    error: MachineCallError<MachineStoragePrepareDomainError>,
) -> MachineStoragePrepareError {
    match error {
        MachineCallError::Unavailable(reason) => MachineStoragePrepareError::Unavailable {
            machine_id: machine_id.clone(),
            reason,
        },
        MachineCallError::Domain(MachineStoragePrepareDomainError::PreparationFailed {
            failure,
        }) => MachineStoragePrepareError::PreparationFailed {
            machine_id: machine_id.clone(),
            failure,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineBuildCachePruneError {
    Unavailable {
        machine_id: MachineId,
        reason: MachineRuntimeUnavailableReason,
    },
    PruneFailed {
        machine_id: MachineId,
        message: ployz_core::operation::FailureMessage,
    },
}

impl MachineSubstrateUpdateError {
    #[must_use]
    pub fn into_operation_failure(self) -> MachineUpdateFailure {
        match self {
            Self::Unavailable { machine_id, reason } => MachineUpdateFailure::MachineUnavailable {
                machine_id,
                message: ployz_core::operation::FailureMessage::try_new(format!("{reason:?}"))
                    .expect("machine runtime unavailable reason is non-empty"),
            },
            Self::UpdateFailed {
                machine_id,
                message,
            } => MachineUpdateFailure::UpdateRejected {
                machine_id,
                message,
            },
        }
    }
}

impl NatsMachineFactsReader {
    pub(crate) async fn read_dataplane_status(
        &self,
        machine_id: &MachineId,
    ) -> Result<MachineDataplaneStatus, ployz_core::operation::FailureMessage> {
        call_machine::<MachineDataplaneStatusRpcOk, MachineDataplaneStatusDomainError>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::DataplaneStatus,
            &MachineDataplaneStatusRpcRequest {
                mode: NetworkStatusMode::Snapshot,
            },
        )
        .await
        .map(|response| response.value)
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => reason.failure_message(),
            MachineCallError::Domain(MachineDataplaneStatusDomainError::ReadFailed { message }) => {
                message
            }
        })
    }
}

impl NatsMachineContainerRuntime {
    #[must_use]
    pub fn new(client: async_nats::Client) -> Self {
        Self {
            client,
            request_timeout: DEFAULT_MACHINE_RPC_TIMEOUT,
        }
    }

    #[must_use]
    pub const fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
    }

    pub async fn inspect_container(
        &self,
        machine_id: &MachineId,
        request: MachineContainerInspectRpcRequest,
    ) -> Result<MachineContainerInspectRpcOk, MachineContainerInspectError> {
        call_machine::<MachineContainerInspectRpcOk, MachineContainerInspectDomainError>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::ContainerInspect,
            &request,
        )
        .await
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => MachineContainerInspectError::Unavailable {
                machine_id: machine_id.clone(),
                reason,
            },
            MachineCallError::Domain(error) => error.into_runtime_error(machine_id.clone()),
        })
    }

    pub async fn remove_volume_reference(
        &self,
        machine_id: &MachineId,
        operation_id: OperationId,
        volume: &VolumePinState,
    ) -> Result<(), MachineVolumeRemoveError> {
        self.remove_volume_effect(
            machine_id,
            MachineVolumeRemoveRpcRequest::DockerReference {
                operation_id,
                volume: volume.clone(),
            },
        )
        .await
    }

    pub async fn destroy_provisioned_volume_dataset(
        &self,
        machine_id: &MachineId,
        operation_id: OperationId,
        volume: ProvisionedVolumePinState,
    ) -> Result<(), MachineVolumeRemoveError> {
        self.remove_volume_effect(
            machine_id,
            MachineVolumeRemoveRpcRequest::ProvisionedDataset {
                operation_id,
                volume,
            },
        )
        .await
    }

    async fn remove_volume_effect(
        &self,
        machine_id: &MachineId,
        request: MachineVolumeRemoveRpcRequest,
    ) -> Result<(), MachineVolumeRemoveError> {
        call_machine::<MachineVolumeRemoveRpcOk, MachineVolumeRemoveDomainError>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::VolumeRemove,
            &request,
        )
        .await
        .map(|_| ())
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => MachineVolumeRemoveError::Unavailable {
                machine_id: machine_id.clone(),
                message: reason.failure_message(),
            },
            MachineCallError::Domain(error) => MachineVolumeRemoveError::Domain {
                machine_id: machine_id.clone(),
                error,
            },
        })
    }

    pub async fn ensure_volume(
        &self,
        machine_id: &MachineId,
        volume: &VolumePinState,
    ) -> Result<(), MachineVolumeEnsureError> {
        call_machine::<MachineVolumeEnsureRpcOk, ployz_core::machine::VolumeEnsureFailure>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::VolumeEnsure,
            &MachineVolumeEnsureRpcRequest {
                volume: volume.clone(),
            },
        )
        .await
        .map(|_| ())
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => MachineVolumeEnsureError::Unavailable {
                machine_id: machine_id.clone(),
                volume_name: volume.volume_name().clone(),
                reason,
            },
            MachineCallError::Domain(failure) => MachineVolumeEnsureError::Domain {
                machine_id: machine_id.clone(),
                volume_name: volume.volume_name().clone(),
                failure,
            },
        })
    }

    pub(crate) async fn request_resolve_image(
        &self,
        machine_id: &MachineId,
        request: &MachineContainerResolveImageRpcRequest,
    ) -> Result<
        MachineContainerResolveImageRpcOk,
        MachineCallError<MachineContainerResolveImageDomainError>,
    > {
        call_machine(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::ContainerResolveImage,
            request,
        )
        .await
    }

    pub(crate) async fn request_ensure_image(
        &self,
        machine_id: &MachineId,
        request: &ImageEnsureRequest,
    ) -> Result<ImageEnsureOk, MachineCallError<ImageRpcDomainError>> {
        call_machine(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::ImageEnsure,
            request,
        )
        .await
    }

    pub(crate) async fn request_remove_image(
        &self,
        machine_id: &MachineId,
        request: &ImageRemoveRequest,
    ) -> Result<ImageRemoveOk, MachineCallError<ployz_core::image::ImageRemoveDomainError>> {
        call_machine(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::ImageRemove,
            request,
        )
        .await
    }

    pub(crate) async fn request_container_run(
        &self,
        machine_id: &MachineId,
        request: &MachineContainerRunRpcRequest,
    ) -> Result<MachineContainerRunRpcOk, MachineCallError<MachineContainerRunDomainError>> {
        call_machine(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::ContainerRun,
            request,
        )
        .await
    }

    pub(crate) async fn request_container_run_hook(
        &self,
        machine_id: &MachineId,
        request: &MachineContainerRunHookRpcRequest,
    ) -> Result<MachineContainerRunHookRpcOk, MachineCallError<MachineContainerRunHookDomainError>>
    {
        call_machine(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::ContainerRunHook,
            request,
        )
        .await
    }

    pub(crate) async fn request_container_remove(
        &self,
        machine_id: &MachineId,
        request: &MachineContainerRemoveRpcRequest,
    ) -> Result<MachineContainerRpcOk, MachineCallError<MachineContainerRemoveDomainError>> {
        call_machine(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::ContainerRemove,
            request,
        )
        .await
    }

    pub(crate) async fn request_container_restart(
        &self,
        machine_id: &MachineId,
        request: &MachineContainerRestartRpcRequest,
    ) -> Result<MachineContainerRpcOk, MachineCallError<MachineContainerRestartDomainError>> {
        call_machine(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::ContainerRestart,
            request,
        )
        .await
    }

    pub(crate) async fn request_container_stop(
        &self,
        machine_id: &MachineId,
        request: &MachineContainerStopRpcRequest,
    ) -> Result<MachineContainerRpcOk, MachineCallError<MachineContainerStopDomainError>> {
        call_machine(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::ContainerStop,
            request,
        )
        .await
    }
}

impl MachineLogsTailDomainError {
    fn into_runtime_error(self, machine_id: MachineId) -> MachineLogsTailError {
        match self {
            Self::NotFound { container_id } => MachineLogsTailError::NotFound {
                machine_id,
                container_id,
            },
            Self::ReadFailed {
                container_id,
                message,
            } => MachineLogsTailError::ReadFailed {
                machine_id,
                container_id,
                message,
            },
        }
    }
}

impl MachineSubstrateUpdateDomainError {
    fn into_runtime_error(self, machine_id: MachineId) -> MachineSubstrateUpdateError {
        match self {
            Self::UpdateFailed { message } => MachineSubstrateUpdateError::UpdateFailed {
                machine_id,
                message,
            },
        }
    }
}

impl MachineContainerInspectDomainError {
    fn into_runtime_error(self, machine_id: MachineId) -> MachineContainerInspectError {
        match self {
            Self::InspectFailed {
                container_id,
                message,
            } => MachineContainerInspectError::InspectFailed {
                machine_id,
                container_id,
                message,
            },
        }
    }
}

impl MachineFactsGetDomainError {
    fn into_runtime_error(self, machine_id: MachineId) -> MachineFactsReadError {
        match self {
            Self::GatherFailed { message } => MachineFactsReadError::GatherFailed {
                machine_id,
                message,
            },
        }
    }
}

pub(super) fn wrong_response_machine(
    requested_machine_id: &MachineId,
    actual_machine_id: MachineId,
) -> Option<MachineRuntimeUnavailableReason> {
    if actual_machine_id == *requested_machine_id {
        return None;
    }

    Some(MachineRuntimeUnavailableReason::WrongResponder { actual_machine_id })
}

pub(crate) fn unavailable_reason(
    error: NatsJsonServiceRequestError,
) -> MachineRuntimeUnavailableReason {
    match error {
        NatsJsonServiceRequestError::EncodeRequest { message } => {
            MachineRuntimeUnavailableReason::EncodeRequest { message }
        }
        NatsJsonServiceRequestError::Request { failure } => machine_request_failure_reason(failure),
        NatsJsonServiceRequestError::Service { failure } => machine_service_failure_reason(failure),
        NatsJsonServiceRequestError::ServiceProtocol { error } => {
            MachineRuntimeUnavailableReason::MalformedServiceError {
                message: error.to_string(),
            }
        }
        NatsJsonServiceRequestError::DecodeResponse { message } => {
            MachineRuntimeUnavailableReason::DecodeResponse { message }
        }
    }
}

fn machine_request_failure_reason(
    failure: NatsServiceRequestFailure,
) -> MachineRuntimeUnavailableReason {
    match failure {
        NatsServiceRequestFailure::TimedOut => MachineRuntimeUnavailableReason::RequestTimedOut,
        NatsServiceRequestFailure::NoResponders => MachineRuntimeUnavailableReason::NoResponders,
        NatsServiceRequestFailure::InvalidSubject => {
            MachineRuntimeUnavailableReason::InvalidSubject
        }
        NatsServiceRequestFailure::MaxPayloadExceeded => {
            MachineRuntimeUnavailableReason::MaxPayloadExceeded
        }
        NatsServiceRequestFailure::Other { message } => {
            MachineRuntimeUnavailableReason::RequestFailed { message }
        }
    }
}

fn machine_service_failure_reason(error: NatsServiceError) -> MachineRuntimeUnavailableReason {
    match error.code {
        NatsServiceErrorCode::BadRequest => MachineRuntimeUnavailableReason::ServiceBadRequest {
            message: error.message,
        },
        NatsServiceErrorCode::Conflict => MachineRuntimeUnavailableReason::ServiceConflict {
            message: error.message,
        },
        NatsServiceErrorCode::ResponseTooLarge => {
            MachineRuntimeUnavailableReason::ServiceResponseTooLarge
        }
        NatsServiceErrorCode::Unavailable => MachineRuntimeUnavailableReason::ServiceUnavailable {
            message: error.message,
        },
        NatsServiceErrorCode::Timeout => MachineRuntimeUnavailableReason::ServiceTimedOut {
            message: error.message,
        },
        NatsServiceErrorCode::Internal => MachineRuntimeUnavailableReason::ServiceInternal {
            message: error.message,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use ployz_core::image::OciPlatform;
    use ployz_core::machine::runtime::{MachineContainerObservationSnapshot, MachineDiskSpace};

    #[test]
    fn placement_answer_binds_seed_clock_to_control_gather_start() {
        let machine_id = MachineId::try_new("machine-a").expect("machine id");
        let facts = MachineFactsSnapshot::try_new(
            machine_id.clone(),
            MachineContainerObservationSnapshot::try_new(machine_id, []).expect("containers"),
            None,
            MachineDiskSpace {
                available_bytes: 1,
                total_bytes: 2,
            },
            None,
            OciPlatform::try_new("linux", "amd64").expect("platform"),
            1_234,
        )
        .expect("facts");

        let answer = placement_answer(
            MachineFactsGetRpcOk {
                facts,
                build: MachineBuildCapability::Available,
            },
            1_000,
        );

        assert_eq!(
            answer.clock,
            MachineClockTestimony {
                control_request_started_at_unix_ms: 1_000,
                machine_observed_at_unix_ms: 1_234,
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn bounded_placement_gather_preserves_answers_and_types_every_unfinished_candidate() {
        let candidates = (0_u8..18)
            .map(|index| {
                (
                    MachineId::try_new(format!("machine-{index:02}"))
                        .expect("candidate machine id"),
                    MachineLifecycle::Active,
                )
            })
            .collect::<Vec<_>>();
        let [(completed_id, _), ..] = candidates.as_slice() else {
            panic!("candidate fixture is non-empty");
        };
        let completed_id = completed_id.clone();
        let completed = MachinePlacementFacts {
            machine_id: completed_id.clone(),
            lifecycle: MachineLifecycle::Active,
            answer: Some(placement_answer(
                MachineFactsGetRpcOk {
                    facts: MachineFactsSnapshot::try_new(
                        completed_id.clone(),
                        MachineContainerObservationSnapshot::try_new(completed_id.clone(), [])
                            .expect("containers"),
                        None,
                        MachineDiskSpace {
                            available_bytes: 1,
                            total_bytes: 2,
                        },
                        None,
                        OciPlatform::try_new("linux", "amd64").expect("platform"),
                        1_234,
                    )
                    .expect("facts"),
                    build: MachineBuildCapability::Available,
                },
                1_000,
            )),
        };
        let reads = stream::once(async move { completed }).chain(stream::pending());

        let gathered =
            collect_machine_placement_facts_until(candidates, reads, Duration::from_secs(2)).await;

        assert_eq!(gathered.len(), 18);
        for facts in gathered {
            if facts.machine_id == completed_id {
                assert!(facts.answer.is_some());
            } else {
                assert_eq!(facts.lifecycle, MachineLifecycle::Active);
                assert!(facts.answer.is_none());
            }
        }
    }
}
