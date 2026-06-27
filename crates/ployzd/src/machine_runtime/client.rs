//! Request-side NATS adapters for machine-local services.

use crate::deploy_worker::{
    MachineContainerRuntime, MachineContainerRuntimeError, MachineRuntimeUnavailableReason,
    WireGuardEbpfPreparer,
};
use crate::machine_runtime::protocol::{
    MachineContainerRemoveDomainError, MachineContainerRemoveRpcRequest, MachineContainerRpcOk,
    MachineContainerRunDomainError, MachineContainerRunRpcOk, MachineContainerRunRpcRequest,
    MachineContainerStopDomainError, MachineContainerStopRpcRequest,
    MachineEnsureEndpointNetworkDomainError, MachineEnsureEndpointNetworkRpcOk,
    MachineEnsureEndpointNetworkRpcRequest, MachineLogsTailDomainError, MachineLogsTailResult,
    MachineLogsTailRpcOk, MachineLogsTailRpcRequest, MachineRpcResponder, MachineRpcResponse,
    MachineRunContainerOutcome, MachineWireGuardEbpfPrepareDomainError,
    MachineWireGuardEbpfPreparePhase, MachineWireGuardEbpfPrepareRpcRequest,
    MachineWireGuardEbpfPrepareRpcResponse,
};
use futures_util::future::try_join_all;
use ployz_core::dataplane::{
    WireGuardEbpfComponent, WireGuardEbpfMachineReady, WireGuardEbpfPrepareError,
    WireGuardEbpfPrepareReport, WireGuardEbpfPrepareReportError, WireGuardEbpfPrepareRequest,
    WireGuardPeer, WireGuardPublicKey,
};
use ployz_core::ids::MachineId;
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_nats::service_protocol::{NatsServiceError, NatsServiceErrorCode};
use ployz_nats::service_runtime::{
    NatsJsonServiceRequestError, NatsServiceRequestFailure, request_json,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::time::Duration;

pub const DEFAULT_MACHINE_RPC_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct NatsMachineContainerRuntime {
    client: async_nats::Client,
    request_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct NatsMachineWireGuardEbpfPreparer {
    client: async_nats::Client,
    request_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct NatsMachineLogsTailer {
    client: async_nats::Client,
    request_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineLogsTailRuntimeError {
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

/// Outcome of one machine RPC round trip: either the machine answered with a typed
/// domain error, or the call never produced a usable answer.
enum MachineCallError<E> {
    Unavailable(MachineRuntimeUnavailableReason),
    Domain(E),
}

/// One machine RPC round trip: encode the request, map transport failures, and
/// reject answers from the wrong machine — exactly once for every endpoint.
async fn call_machine<T, E>(
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
    ) -> Result<MachineLogsTailResult, MachineLogsTailRuntimeError> {
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
            MachineCallError::Unavailable(reason) => MachineLogsTailRuntimeError::Unavailable {
                machine_id: machine_id.clone(),
                reason,
            },
            MachineCallError::Domain(error) => error.into_runtime_error(machine_id.clone()),
        })
    }
}

impl NatsMachineWireGuardEbpfPreparer {
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
    fn into_runtime_error(self, machine_id: MachineId) -> MachineLogsTailRuntimeError {
        match self {
            Self::NotFound { container_id } => MachineLogsTailRuntimeError::NotFound {
                machine_id,
                container_id,
            },
            Self::ReadFailed {
                container_id,
                message,
            } => MachineLogsTailRuntimeError::ReadFailed {
                machine_id,
                container_id,
                message,
            },
        }
    }
}

impl MachineContainerRunDomainError {
    fn into_runtime_error(self, machine_id: MachineId) -> MachineContainerRuntimeError {
        match self {
            Self::OperationStepConflict {
                container_id,
                expected,
                actual,
            } => MachineContainerRuntimeError::OperationStepConflict {
                machine_id,
                container_id,
                expected,
                actual,
            },
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

impl WireGuardEbpfPreparer for NatsMachineWireGuardEbpfPreparer {
    async fn prepare_wireguard_ebpf(
        &mut self,
        request: WireGuardEbpfPrepareRequest,
    ) -> Result<WireGuardEbpfPrepareReport, WireGuardEbpfPrepareError> {
        let peers = if request.peers.is_empty() && request.machines.len() > 1 {
            let rpc_request = read_public_key_request(&request);
            let public_keys = try_join_all(request.machines.iter().map(|machine_id| {
                read_machine_wireguard_public_key(self, machine_id, &rpc_request)
            }))
            .await?;
            wireguard_peers_from_public_keys(&request, &public_keys)?
        } else {
            request.peers.clone()
        };

        let final_request = request.with_peers(peers);
        let rpc_request = MachineWireGuardEbpfPrepareRpcRequest::from(final_request.clone());
        let machines = try_join_all(
            final_request
                .machines
                .iter()
                .map(|machine_id| prepare_machine_wireguard_ebpf(self, machine_id, &rpc_request)),
        )
        .await?;

        WireGuardEbpfPrepareReport::for_request(&final_request, machines)
            .map_err(wireguard_ebpf_report_error)
    }
}

fn read_public_key_request(
    request: &WireGuardEbpfPrepareRequest,
) -> MachineWireGuardEbpfPrepareRpcRequest {
    MachineWireGuardEbpfPrepareRpcRequest {
        phase: MachineWireGuardEbpfPreparePhase::ReadPublicKey,
        operation_id: request.operation_id.clone(),
        machines: request.machines.clone(),
        endpoint_routes: Vec::new(),
        peer_endpoints: Vec::new(),
        peers: Vec::new(),
    }
}

fn wireguard_peers_from_public_keys(
    request: &WireGuardEbpfPrepareRequest,
    public_keys: &[(MachineId, WireGuardPublicKey)],
) -> Result<Vec<WireGuardPeer>, WireGuardEbpfPrepareError> {
    if request.machines.len() < 2 {
        return Ok(Vec::new());
    }

    let mut peers = Vec::new();
    for machine_id in &request.machines {
        let Some(endpoint) = request
            .peer_endpoints
            .iter()
            .find(|endpoint| endpoint.machine_id == *machine_id)
            .cloned()
        else {
            return Err(invalid_wireguard_report(format!(
                "wireguard endpoint is missing for {}",
                machine_id.as_str()
            )));
        };
        let Some((_, public_key)) = public_keys
            .iter()
            .find(|(ready_machine_id, _)| ready_machine_id == machine_id)
        else {
            return Err(invalid_wireguard_report(format!(
                "wireguard public key is missing for {}",
                machine_id.as_str()
            )));
        };
        peers.push(WireGuardPeer::from_endpoint(endpoint, public_key.clone()));
    }

    Ok(peers)
}

async fn read_machine_wireguard_public_key(
    preparer: &NatsMachineWireGuardEbpfPreparer,
    machine_id: &MachineId,
    request: &MachineWireGuardEbpfPrepareRpcRequest,
) -> Result<(MachineId, WireGuardPublicKey), WireGuardEbpfPrepareError> {
    let subject = machine_service(machine_id, MachineServiceEndpoint::WireGuardEbpfPrepare);
    let response = request_json::<_, MachineWireGuardEbpfPrepareRpcResponse>(
        &preparer.client,
        subject,
        request,
        preparer.request_timeout,
    )
    .await
    .map_err(|error| wireguard_ebpf_request_error(machine_id, error))?;

    match response {
        MachineWireGuardEbpfPrepareRpcResponse::PublicKey {
            machine_id: actual_machine_id,
            public_key,
        } => match wrong_response_machine(machine_id, actual_machine_id.clone()) {
            Some(reason) => Err(wireguard_ebpf_unavailable(
                machine_id,
                reason.failure_message(),
            )),
            None => Ok((actual_machine_id, public_key)),
        },
        MachineWireGuardEbpfPrepareRpcResponse::Ok { .. } => {
            Err(invalid_wireguard_report(format!(
                "machine {} returned dataplane readiness for public key request",
                machine_id.as_str()
            )))
        }
        MachineWireGuardEbpfPrepareRpcResponse::DomainError {
            machine_id: actual_machine_id,
            error,
        } => match wrong_response_machine(machine_id, actual_machine_id) {
            Some(reason) => Err(wireguard_ebpf_unavailable(
                machine_id,
                reason.failure_message(),
            )),
            None => Err(error.into_prepare_error(machine_id.clone())),
        },
    }
}

async fn prepare_machine_wireguard_ebpf(
    preparer: &NatsMachineWireGuardEbpfPreparer,
    machine_id: &MachineId,
    request: &MachineWireGuardEbpfPrepareRpcRequest,
) -> Result<WireGuardEbpfMachineReady, WireGuardEbpfPrepareError> {
    let subject = machine_service(machine_id, MachineServiceEndpoint::WireGuardEbpfPrepare);
    let response = request_json::<_, MachineWireGuardEbpfPrepareRpcResponse>(
        &preparer.client,
        subject,
        request,
        preparer.request_timeout,
    )
    .await
    .map_err(|error| wireguard_ebpf_request_error(machine_id, error))?;

    match response {
        MachineWireGuardEbpfPrepareRpcResponse::Ok { readiness } => {
            match wrong_response_machine(machine_id, readiness.machine_id.clone()) {
                Some(reason) => Err(wireguard_ebpf_unavailable(
                    machine_id,
                    reason.failure_message(),
                )),
                None => Ok(readiness),
            }
        }
        MachineWireGuardEbpfPrepareRpcResponse::PublicKey { .. } => {
            Err(invalid_wireguard_report(format!(
                "machine {} returned public key for dataplane prepare request",
                machine_id.as_str()
            )))
        }
        MachineWireGuardEbpfPrepareRpcResponse::DomainError {
            machine_id: actual_machine_id,
            error,
        } => match wrong_response_machine(machine_id, actual_machine_id) {
            Some(reason) => Err(wireguard_ebpf_unavailable(
                machine_id,
                reason.failure_message(),
            )),
            None => Err(error.into_prepare_error(machine_id.clone())),
        },
    }
}

impl MachineWireGuardEbpfPrepareDomainError {
    fn into_prepare_error(self, machine_id: MachineId) -> WireGuardEbpfPrepareError {
        match self {
            Self::Unavailable { component, message } => WireGuardEbpfPrepareError::Unavailable {
                machine_id,
                component,
                message,
            },
        }
    }
}

fn wrong_response_machine(
    requested_machine_id: &MachineId,
    actual_machine_id: MachineId,
) -> Option<MachineRuntimeUnavailableReason> {
    if actual_machine_id == *requested_machine_id {
        return None;
    }

    Some(MachineRuntimeUnavailableReason::WrongResponder { actual_machine_id })
}

fn unavailable_reason(error: NatsJsonServiceRequestError) -> MachineRuntimeUnavailableReason {
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

fn wireguard_ebpf_request_error(
    machine_id: &MachineId,
    error: NatsJsonServiceRequestError,
) -> WireGuardEbpfPrepareError {
    wireguard_ebpf_unavailable(machine_id, unavailable_reason(error).failure_message())
}

fn wireguard_ebpf_unavailable(
    machine_id: &MachineId,
    message: ployz_core::ops::FailureMessage,
) -> WireGuardEbpfPrepareError {
    WireGuardEbpfPrepareError::Unavailable {
        machine_id: machine_id.clone(),
        component: WireGuardEbpfComponent::WireGuard,
        message,
    }
}

fn wireguard_ebpf_report_error(
    error: WireGuardEbpfPrepareReportError,
) -> WireGuardEbpfPrepareError {
    let message = match error {
        WireGuardEbpfPrepareReportError::Empty => "wireguard/eBPF report had no machines",
        WireGuardEbpfPrepareReportError::DuplicateMachine => {
            "wireguard/eBPF report contained duplicate machines"
        }
        WireGuardEbpfPrepareReportError::MachineSetMismatch => {
            "wireguard/eBPF report did not match requested machines"
        }
    };
    WireGuardEbpfPrepareError::InvalidReport {
        message: ployz_core::ops::FailureMessage::try_new(message)
            .expect("generated dataplane failure message is non-empty"),
    }
}

fn invalid_wireguard_report(message: String) -> WireGuardEbpfPrepareError {
    WireGuardEbpfPrepareError::InvalidReport {
        message: ployz_core::ops::FailureMessage::try_new(message)
            .expect("generated dataplane failure message is non-empty"),
    }
}
