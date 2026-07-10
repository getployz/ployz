//! Request-side adapter for the built-in Ployz Native Mesh provider.

use crate::operations::deploy::DataplanePreparer;
use crate::roles::machine::client::{
    MachineCallError, MachineFactsReadError, NatsMachineDataplanePreparer, call_machine,
};
use crate::roles::machine::protocol::{
    MachineDataplanePrepareRpcRequest, MachinePloyzNativeMeshPrepareDomainError,
    MachinePloyzNativeMeshPrepareRpcOk, MachinePloyzNativeMeshPrepareRpcRequest,
};
use futures_util::future::try_join_all;
use ployz_core::dataplane::{
    DataplaneMember, DataplanePrepareError, DataplanePrepareRequest,
    OVERLAY_CONNECTIVITY_PROOF_BUDGET, PloyzNativeMeshComponent, PloyzNativeMeshMachineReady,
    PloyzNativeMeshPrepareReport, PloyzNativeMeshPrepareRequest, WireGuardEbpfPrepareError,
    WireGuardEbpfPrepareReportError, WireGuardPeer, WireGuardPeerEndpoint, WireGuardPublicKey,
};
use ployz_core::ids::MachineId;
use ployz_core::subjects::MachineServiceEndpoint;
use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::time::Duration;

const OVERLAY_CONNECTIVITY_PROBE_RPC_TIMEOUT: Duration =
    OVERLAY_CONNECTIVITY_PROOF_BUDGET.saturating_add(Duration::from_secs(15));

impl DataplanePreparer for NatsMachineDataplanePreparer {
    async fn prepare_dataplane(
        &mut self,
        request: DataplanePrepareRequest,
    ) -> Result<PloyzNativeMeshPrepareReport, DataplanePrepareError> {
        let request = self
            .ployz_native_mesh_prepare_request(request)
            .await
            .map_err(DataplanePrepareError::from)?;
        let report = self
            .prepare_ployz_native_mesh(request)
            .await
            .map_err(DataplanePrepareError::from)?;
        Ok(report)
    }
}

impl NatsMachineDataplanePreparer {
    pub async fn probe_overlay(
        &self,
        operation_id: ployz_core::ids::OperationId,
        machine_id: &MachineId,
        machines: Vec<MachineId>,
        targets: Vec<Ipv4Addr>,
    ) -> Result<Vec<Ipv4Addr>, WireGuardEbpfPrepareError> {
        let request = MachineDataplanePrepareRpcRequest::ployz_native_mesh(
            operation_id,
            machines,
            MachinePloyzNativeMeshPrepareRpcRequest::ProbeOverlay {
                targets: targets.clone(),
            },
        );
        let response = request_machine_dataplane_with_timeout(
            self,
            machine_id,
            &request,
            OVERLAY_CONNECTIVITY_PROBE_RPC_TIMEOUT,
        )
        .await?;
        match response {
            MachinePloyzNativeMeshPrepareRpcOk::OverlayProbe { unreachable, .. } => {
                if unreachable.iter().all(|target| targets.contains(target)) {
                    Ok(unreachable)
                } else {
                    Err(invalid_ployz_native_mesh_report(format!(
                        "machine {} returned an overlay probe target that was not requested",
                        machine_id.as_str()
                    )))
                }
            }
            MachinePloyzNativeMeshPrepareRpcOk::PublicKey { .. } => {
                Err(invalid_ployz_native_mesh_report(format!(
                    "machine {} returned public key for overlay probe request",
                    machine_id.as_str()
                )))
            }
            MachinePloyzNativeMeshPrepareRpcOk::Ready { .. } => {
                Err(invalid_ployz_native_mesh_report(format!(
                    "machine {} returned dataplane readiness for overlay probe request",
                    machine_id.as_str()
                )))
            }
        }
    }

    async fn ployz_native_mesh_prepare_request(
        &self,
        request: DataplanePrepareRequest,
    ) -> Result<PloyzNativeMeshPrepareRequest, WireGuardEbpfPrepareError> {
        validate_ployz_native_mesh_membership(&request.membership)?;
        let peer_endpoints = self
            .load_wireguard_peer_endpoints(&request.membership)
            .await?;
        Ok(PloyzNativeMeshPrepareRequest::from_dataplane_request(
            request,
            peer_endpoints,
        ))
    }

    async fn load_wireguard_peer_endpoints(
        &self,
        membership: &[DataplaneMember],
    ) -> Result<Vec<WireGuardPeerEndpoint>, WireGuardEbpfPrepareError> {
        if membership.len() < 2 {
            return Ok(Vec::new());
        }

        let mut endpoints = try_join_all(
            membership
                .iter()
                .map(|member| load_wireguard_peer_endpoint(self, member)),
        )
        .await?;
        endpoints.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
        Ok(endpoints)
    }

    async fn prepare_ployz_native_mesh(
        &self,
        request: PloyzNativeMeshPrepareRequest,
    ) -> Result<PloyzNativeMeshPrepareReport, WireGuardEbpfPrepareError> {
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
        let rpc_request = MachineDataplanePrepareRpcRequest::from(final_request.clone());
        let machines =
            try_join_all(final_request.machines.iter().map(|machine_id| {
                prepare_machine_ployz_native_mesh(self, machine_id, &rpc_request)
            }))
            .await?;

        PloyzNativeMeshPrepareReport::for_request(&final_request, machines)
            .map_err(ployz_native_mesh_report_error)
    }
}

async fn load_wireguard_peer_endpoint(
    preparer: &NatsMachineDataplanePreparer,
    member: &DataplaneMember,
) -> Result<WireGuardPeerEndpoint, WireGuardEbpfPrepareError> {
    let facts = preparer
        .facts_reader
        .machine_facts(&member.machine_id)
        .await
        .map_err(|error| public_ip_fact_unavailable(&member.machine_id, error))?;
    let endpoints = facts.endpoints().cloned().ok_or_else(|| {
        ployz_native_mesh_unavailable(
            &member.machine_id,
            PloyzNativeMeshComponent::WireGuard,
            failure_message(format!(
                "machine endpoint facts for {} are missing",
                member.machine_id.as_str()
            )),
        )
    })?;
    peer_endpoint_from_observation(member, endpoints)
}

fn public_ip_fact_unavailable(
    machine_id: &MachineId,
    error: MachineFactsReadError,
) -> WireGuardEbpfPrepareError {
    ployz_native_mesh_unavailable(
        machine_id,
        PloyzNativeMeshComponent::WireGuard,
        failure_message(format!("machine public ip fact could not be read: {error}")),
    )
}

fn validate_ployz_native_mesh_membership(
    membership: &[DataplaneMember],
) -> Result<(), WireGuardEbpfPrepareError> {
    let mut machine_ids = BTreeSet::new();
    let mut endpoint_subnets = BTreeSet::new();
    for member in membership {
        if !machine_ids.insert(member.machine_id.clone()) {
            return Err(ployz_native_mesh_unavailable(
                &member.machine_id,
                PloyzNativeMeshComponent::WireGuard,
                failure_message(format!(
                    "ployz native mesh membership contained duplicate machine: {}",
                    member.machine_id.as_str()
                )),
            ));
        }
        if !endpoint_subnets.insert(member.endpoint_subnet.clone()) {
            return Err(ployz_native_mesh_unavailable(
                &member.machine_id,
                PloyzNativeMeshComponent::WireGuard,
                failure_message(format!(
                    "ployz native mesh endpoint subnet is already assigned: {}",
                    member.endpoint_subnet.as_string()
                )),
            ));
        }
    }
    Ok(())
}

fn peer_endpoint_from_observation(
    member: &DataplaneMember,
    observation: ployz_core::state::MachineEndpointObservation,
) -> Result<WireGuardPeerEndpoint, WireGuardEbpfPrepareError> {
    let Some(active_endpoint) = observation.mesh_endpoints.first().copied() else {
        return Err(ployz_native_mesh_unavailable(
            &member.machine_id,
            PloyzNativeMeshComponent::WireGuard,
            failure_message(format!(
                "machine mesh endpoints for {} are missing",
                member.machine_id.as_str()
            )),
        ));
    };
    Ok(WireGuardPeerEndpoint {
        machine_id: observation.machine_id,
        endpoint_subnet: member.endpoint_subnet.as_string(),
        active_endpoint,
        candidate_endpoints: observation.mesh_endpoints,
    })
}

fn read_public_key_request(
    request: &PloyzNativeMeshPrepareRequest,
) -> MachineDataplanePrepareRpcRequest {
    MachineDataplanePrepareRpcRequest::ployz_native_mesh(
        request.operation_id.clone(),
        request.machines.clone(),
        MachinePloyzNativeMeshPrepareRpcRequest::ReadPublicKey,
    )
}

fn wireguard_peers_from_public_keys(
    request: &PloyzNativeMeshPrepareRequest,
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
            return Err(invalid_ployz_native_mesh_report(format!(
                "wireguard endpoint is missing for {}",
                machine_id.as_str()
            )));
        };
        let Some((_, public_key)) = public_keys
            .iter()
            .find(|(ready_machine_id, _)| ready_machine_id == machine_id)
        else {
            return Err(invalid_ployz_native_mesh_report(format!(
                "wireguard public key is missing for {}",
                machine_id.as_str()
            )));
        };
        peers.push(WireGuardPeer::from_endpoint(endpoint, public_key.clone()));
    }

    Ok(peers)
}

async fn read_machine_wireguard_public_key(
    preparer: &NatsMachineDataplanePreparer,
    machine_id: &MachineId,
    request: &MachineDataplanePrepareRpcRequest,
) -> Result<(MachineId, WireGuardPublicKey), WireGuardEbpfPrepareError> {
    let response = request_machine_dataplane(preparer, machine_id, request).await?;
    match response {
        MachinePloyzNativeMeshPrepareRpcOk::PublicKey {
            machine_id: actual_machine_id,
            public_key,
        } => Ok((actual_machine_id, public_key)),
        MachinePloyzNativeMeshPrepareRpcOk::Ready { .. } => {
            Err(invalid_ployz_native_mesh_report(format!(
                "machine {} returned dataplane readiness for public key request",
                machine_id.as_str()
            )))
        }
        MachinePloyzNativeMeshPrepareRpcOk::OverlayProbe { .. } => {
            Err(invalid_ployz_native_mesh_report(format!(
                "machine {} returned overlay probe for public key request",
                machine_id.as_str()
            )))
        }
    }
}

async fn prepare_machine_ployz_native_mesh(
    preparer: &NatsMachineDataplanePreparer,
    machine_id: &MachineId,
    request: &MachineDataplanePrepareRpcRequest,
) -> Result<PloyzNativeMeshMachineReady, WireGuardEbpfPrepareError> {
    let response = request_machine_dataplane(preparer, machine_id, request).await?;
    match response {
        MachinePloyzNativeMeshPrepareRpcOk::Ready { readiness } => Ok(readiness),
        MachinePloyzNativeMeshPrepareRpcOk::PublicKey { .. } => {
            Err(invalid_ployz_native_mesh_report(format!(
                "machine {} returned public key for dataplane prepare request",
                machine_id.as_str()
            )))
        }
        MachinePloyzNativeMeshPrepareRpcOk::OverlayProbe { .. } => {
            Err(invalid_ployz_native_mesh_report(format!(
                "machine {} returned overlay probe for dataplane prepare request",
                machine_id.as_str()
            )))
        }
    }
}

async fn request_machine_dataplane(
    preparer: &NatsMachineDataplanePreparer,
    machine_id: &MachineId,
    request: &MachineDataplanePrepareRpcRequest,
) -> Result<MachinePloyzNativeMeshPrepareRpcOk, WireGuardEbpfPrepareError> {
    request_machine_dataplane_with_timeout(preparer, machine_id, request, preparer.request_timeout)
        .await
}

async fn request_machine_dataplane_with_timeout(
    preparer: &NatsMachineDataplanePreparer,
    machine_id: &MachineId,
    request: &MachineDataplanePrepareRpcRequest,
    request_timeout: Duration,
) -> Result<MachinePloyzNativeMeshPrepareRpcOk, WireGuardEbpfPrepareError> {
    call_machine::<MachinePloyzNativeMeshPrepareRpcOk, MachinePloyzNativeMeshPrepareDomainError>(
        &preparer.client,
        request_timeout,
        machine_id,
        MachineServiceEndpoint::DataplanePrepare,
        request,
    )
    .await
    .map_err(|error| match error {
        MachineCallError::Unavailable(reason) => ployz_native_mesh_unavailable(
            machine_id,
            PloyzNativeMeshComponent::WireGuard,
            reason.failure_message(),
        ),
        MachineCallError::Domain(error) => error.into_prepare_error(machine_id.clone()),
    })
}

impl MachinePloyzNativeMeshPrepareDomainError {
    fn into_prepare_error(self, machine_id: MachineId) -> WireGuardEbpfPrepareError {
        match self {
            Self::Unavailable { component, message } => WireGuardEbpfPrepareError::Unavailable {
                machine_id,
                component,
                message,
            },
            Self::InvalidReport { message } => WireGuardEbpfPrepareError::InvalidReport { message },
        }
    }
}

fn ployz_native_mesh_unavailable(
    machine_id: &MachineId,
    component: PloyzNativeMeshComponent,
    message: ployz_core::ops::FailureMessage,
) -> WireGuardEbpfPrepareError {
    WireGuardEbpfPrepareError::Unavailable {
        machine_id: machine_id.clone(),
        component,
        message,
    }
}

fn ployz_native_mesh_report_error(
    error: WireGuardEbpfPrepareReportError,
) -> WireGuardEbpfPrepareError {
    let message = match error {
        WireGuardEbpfPrepareReportError::Empty => "ployz native mesh report had no machines",
        WireGuardEbpfPrepareReportError::DuplicateMachine => {
            "ployz native mesh report contained duplicate machines"
        }
        WireGuardEbpfPrepareReportError::MachineSetMismatch => {
            "ployz native mesh report did not match requested machines"
        }
    };
    WireGuardEbpfPrepareError::InvalidReport {
        message: ployz_core::ops::FailureMessage::try_new(message)
            .expect("generated dataplane failure message is non-empty"),
    }
}

fn invalid_ployz_native_mesh_report(message: String) -> WireGuardEbpfPrepareError {
    WireGuardEbpfPrepareError::InvalidReport {
        message: failure_message(message),
    }
}

fn failure_message(message: impl Into<String>) -> ployz_core::ops::FailureMessage {
    ployz_core::ops::FailureMessage::try_new(message)
        .expect("generated dataplane failure message is non-empty")
}
