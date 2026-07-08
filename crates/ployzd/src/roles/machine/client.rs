//! Request-side NATS adapters for machine-local services.

use crate::operations::deploy::{
    MachineContainerRuntime, MachineContainerRuntimeError, MachineRuntimeUnavailableReason,
};
use crate::roles::machine::protocol::{
    MachineContainerInspectDomainError, MachineContainerInspectRpcOk,
    MachineContainerInspectRpcRequest, MachineContainerRemoveDomainError,
    MachineContainerRemoveRpcRequest, MachineContainerRpcOk, MachineContainerRunDomainError,
    MachineContainerRunRpcOk, MachineContainerRunRpcRequest, MachineContainerStopDomainError,
    MachineContainerStopRpcRequest, MachineEnsureEndpointNetworkDomainError,
    MachineEnsureEndpointNetworkRpcOk, MachineEnsureEndpointNetworkRpcRequest,
    MachineFactsGetDomainError, MachineFactsGetRpcOk, MachineFactsGetRpcRequest,
    MachineLogsTailDomainError, MachineLogsTailResult, MachineLogsTailRpcOk,
    MachineLogsTailRpcRequest, MachineRpcResponder, MachineRpcResponse, MachineRunContainerOutcome,
    MachineSubstrateReportRpcOk, MachineSubstrateReportRpcRequest,
    MachineSubstrateUpdateDomainError, MachineSubstrateUpdateRpcOk,
    MachineSubstrateUpdateRpcRequest,
};
use futures_util::{StreamExt, stream};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine_runtime::{
    MachineContainerObservationSnapshot, MachineFactsSnapshot, ManagedContainerObservation,
};
use ployz_core::ops::{MachineSubstrateVersions, MachineUpdateFailure};
use ployz_core::state::MachineLifecycle;
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_nats::service_protocol::{NatsServiceError, NatsServiceErrorCode};
use ployz_nats::service_runtime::{
    NatsJsonServiceRequestError, NatsServiceRequestFailure, request_json,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::time::Duration;

pub const DEFAULT_MACHINE_RPC_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONCURRENT_MACHINE_READS: usize = 16;

#[derive(Debug, Clone)]
pub struct NatsMachineContainerRuntime {
    client: async_nats::Client,
    request_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct NatsMachineDataplanePreparer {
    pub(super) client: async_nats::Client,
    pub(super) facts_reader: NatsMachineFactsReader,
    pub(super) request_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct NatsMachineFactsReader {
    client: async_nats::Client,
    request_timeout: Duration,
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
pub enum MachineSubstrateUpdateError {
    Unavailable {
        machine_id: MachineId,
        reason: MachineRuntimeUnavailableReason,
    },
    UpdateFailed {
        machine_id: MachineId,
        message: ployz_core::ops::FailureMessage,
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
        message: ployz_core::ops::FailureMessage,
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
        message: ployz_core::ops::FailureMessage,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineContainerInspectError {
    InspectFailed {
        machine_id: MachineId,
        container_id: ployz_core::ids::ContainerId,
        message: ployz_core::ops::FailureMessage,
    },
    Unavailable {
        machine_id: MachineId,
        reason: MachineRuntimeUnavailableReason,
    },
}

/// Outcome of one machine RPC round trip: either the machine answered with a typed
/// domain error, or the call never produced a usable answer.
pub(super) enum MachineCallError<E> {
    Unavailable(MachineRuntimeUnavailableReason),
    Domain(E),
}

/// One machine RPC round trip: encode the request, map transport failures, and
/// reject answers from the wrong machine — exactly once for every endpoint.
pub(super) async fn call_machine<T, E>(
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

    #[must_use]
    pub const fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
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
        call_machine::<MachineFactsGetRpcOk, MachineFactsGetDomainError>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::FactsGet,
            &MachineFactsGetRpcRequest {},
        )
        .await
        .map(|ok| ok.facts)
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => MachineFactsReadError::Unavailable {
                machine_id: machine_id.clone(),
                reason,
            },
            MachineCallError::Domain(error) => error.into_runtime_error(machine_id.clone()),
        })
    }
}

pub(crate) struct MachinePlacementFacts {
    pub machine_id: MachineId,
    pub lifecycle: MachineLifecycle,
    pub containers: Option<MachineContainerObservationSnapshot>,
}

pub(crate) async fn read_machine_placement_facts(
    facts_reader: &NatsMachineFactsReader,
    machine_lifecycles: impl IntoIterator<Item = (MachineId, MachineLifecycle)>,
) -> Vec<MachinePlacementFacts> {
    let mut reads = stream::iter(machine_lifecycles)
        .map(|(machine_id, lifecycle)| async move {
            let facts = facts_reader.machine_facts(&machine_id).await;
            MachinePlacementFacts {
                machine_id,
                lifecycle,
                containers: facts.ok().map(|facts| facts.containers().clone()),
            }
        })
        .buffer_unordered(MAX_CONCURRENT_MACHINE_READS);

    let mut facts = Vec::new();
    while let Some(machine) = reads.next().await {
        facts.push(machine);
    }
    facts.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
    facts
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

    #[must_use]
    pub const fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
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
}

impl MachineSubstrateUpdateError {
    #[must_use]
    pub fn into_operation_failure(self) -> MachineUpdateFailure {
        match self {
            Self::Unavailable { machine_id, reason } => MachineUpdateFailure::MachineUnavailable {
                machine_id,
                message: ployz_core::ops::FailureMessage::try_new(format!("{reason:?}"))
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

impl NatsMachineDataplanePreparer {
    #[must_use]
    pub fn new(client: async_nats::Client) -> Self {
        Self {
            facts_reader: NatsMachineFactsReader::new(client.clone()),
            client,
            request_timeout: DEFAULT_MACHINE_RPC_TIMEOUT,
        }
    }

    #[must_use]
    pub fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self.facts_reader = self.facts_reader.with_request_timeout(request_timeout);
        self
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
    ) -> Result<Option<ManagedContainerObservation>, MachineContainerInspectError> {
        call_machine::<MachineContainerInspectRpcOk, MachineContainerInspectDomainError>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::ContainerInspect,
            &request,
        )
        .await
        .map(|ok| ok.observation)
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => MachineContainerInspectError::Unavailable {
                machine_id: machine_id.clone(),
                reason,
            },
            MachineCallError::Domain(error) => error.into_runtime_error(machine_id.clone()),
        })
    }
}

impl MachineContainerRuntime for NatsMachineContainerRuntime {
    async fn ensure_endpoint_network(
        &mut self,
        machine_id: &MachineId,
        request: MachineEnsureEndpointNetworkRpcRequest,
    ) -> Result<(), MachineContainerRuntimeError> {
        call_machine::<MachineEnsureEndpointNetworkRpcOk, MachineEnsureEndpointNetworkDomainError>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::ContainerEnsureEndpointNetwork,
            &request,
        )
        .await
        .map(|_| ())
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => {
                container_runtime_unavailable(machine_id, reason)
            }
            MachineCallError::Domain(error) => error.into_runtime_error(machine_id.clone()),
        })
    }

    async fn run_container(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRunRpcRequest,
    ) -> Result<MachineRunContainerOutcome, MachineContainerRuntimeError> {
        call_machine::<MachineContainerRunRpcOk, MachineContainerRunDomainError>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::ContainerRun,
            &request,
        )
        .await
        .map(|ok| ok.outcome)
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => {
                container_runtime_unavailable(machine_id, reason)
            }
            MachineCallError::Domain(error) => error.into_runtime_error(machine_id.clone()),
        })
    }

    async fn remove_container(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRemoveRpcRequest,
    ) -> Result<(), MachineContainerRuntimeError> {
        call_machine::<MachineContainerRpcOk, MachineContainerRemoveDomainError>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::ContainerRemove,
            &request,
        )
        .await
        .map(|_| ())
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => {
                container_runtime_unavailable(machine_id, reason)
            }
            MachineCallError::Domain(error) => error.into_runtime_error(machine_id.clone()),
        })
    }

    async fn stop_container(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerStopRpcRequest,
    ) -> Result<(), MachineContainerRuntimeError> {
        call_machine::<MachineContainerRpcOk, MachineContainerStopDomainError>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::ContainerStop,
            &request,
        )
        .await
        .map(|_| ())
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => {
                container_runtime_unavailable(machine_id, reason)
            }
            MachineCallError::Domain(error) => error.into_runtime_error(machine_id.clone()),
        })
    }
}

fn container_runtime_unavailable(
    machine_id: &MachineId,
    reason: MachineRuntimeUnavailableReason,
) -> MachineContainerRuntimeError {
    MachineContainerRuntimeError::Unavailable {
        machine_id: machine_id.clone(),
        reason,
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

impl MachineContainerRunDomainError {
    fn into_runtime_error(self, machine_id: MachineId) -> MachineContainerRuntimeError {
        match self {
            Self::OperationStepAmbiguous {
                operation_id,
                step_id,
                container_ids,
            } => MachineContainerRuntimeError::OperationStepAmbiguous {
                machine_id,
                operation_id,
                step_id,
                container_ids,
            },
            Self::CreatedContainerStartFailed {
                container_id,
                message,
                inspect_hint,
            } => MachineContainerRuntimeError::CreatedContainerStartFailed {
                machine_id,
                container_id,
                message,
                inspect_hint,
            },
            Self::ExistingContainerStartFailed {
                container_id,
                message,
                inspect_hint,
            } => MachineContainerRuntimeError::ExistingContainerStartFailed {
                machine_id,
                container_id,
                message,
                inspect_hint,
            },
            Self::OperationStepContainerNotStartable {
                container_id,
                message,
                inspect_hint,
            } => MachineContainerRuntimeError::OperationStepContainerNotStartable {
                machine_id,
                container_id,
                message,
                inspect_hint,
            },
        }
    }
}

impl MachineEnsureEndpointNetworkDomainError {
    fn into_runtime_error(self, machine_id: MachineId) -> MachineContainerRuntimeError {
        match self {
            Self::EnsureFailed { message } => MachineContainerRuntimeError::Unavailable {
                machine_id,
                reason: MachineRuntimeUnavailableReason::ServiceUnavailable {
                    message: message.as_str().to_owned(),
                },
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

impl MachineContainerRemoveDomainError {
    fn into_runtime_error(self, machine_id: MachineId) -> MachineContainerRuntimeError {
        match self {
            Self::RemoveFailed {
                container_id,
                message,
                inspect_hint,
            } => MachineContainerRuntimeError::RemoveContainerFailed {
                machine_id,
                container_id,
                message,
                inspect_hint,
            },
        }
    }
}

impl MachineContainerStopDomainError {
    fn into_runtime_error(self, machine_id: MachineId) -> MachineContainerRuntimeError {
        match self {
            Self::StopFailed {
                container_id,
                message,
                inspect_hint,
            } => MachineContainerRuntimeError::StopContainerFailed {
                machine_id,
                container_id,
                message,
                inspect_hint,
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

pub(super) fn unavailable_reason(
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
