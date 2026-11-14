//! What this Machine reads of its mesh peers: Corrosion's membership, a peer's
//! store versions, and a peer's own answer for a Container. A peer's replicated
//! row counts only when its owner answers In at the row's gen, so the replicated
//! observation narrows what this Machine does and never authorizes it. This
//! module imports neither the Machine nor the boot pass, so both import it.

use std::{
    collections::BTreeMap,
    io,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use ployz_core::{
    CodecError, ContainerDetails, ContainerId, ContainerObservation, CopyRotation,
    InspectContainerRequest, ListMachinesRequest, MACHINE_API_PORT, Machine, MachineId,
    MachineRpcClient, ManagementAddress, MembershipEvidence, RUNTIME_WATCH_MESSAGE_SIZE_LIMIT,
    RequestedServiceSpec, ResolvedServiceSpec, SpecChange, compare_specs, op,
};
use tonic::transport::Endpoint;

use crate::corrosion::{
    AdminClient, Error as CorrosionError, ReplicatedObservations, membership_states_by_address,
};

pub(crate) type MembershipStates = BTreeMap<ManagementAddress, MembershipEvidence>;

/// Corrosion's membership as the local admin socket reports it, read within
/// `bound`. Each caller decides what an unreadable sample means for it.
pub(crate) async fn sample_membership(
    admin: &AdminClient,
    bound: Duration,
) -> Result<MembershipStates, CorrosionError> {
    let states = tokio::time::timeout(bound, admin.membership_states())
        .await
        .map_err(|_| {
            CorrosionError::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                "membership sample timed out",
            ))
        })??;
    Ok(membership_states_by_address(states))
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PeerError {
    #[error("could not reach the peer")]
    Connect(#[from] tonic::transport::Error),
    #[error("the peer refused the request")]
    Rpc(#[from] tonic::Status),
    #[error("the peer's answer did not decode")]
    Codec(#[from] CodecError),
    #[error("the peer's answer carried no store versions")]
    NoEnrollment,
}

/// A request with no iroh connection skips the Machine API's credential check.
pub(crate) async fn target_versions(
    address: SocketAddr,
) -> Result<BTreeMap<String, i64>, PeerError> {
    let channel = Endpoint::from_shared(format!("http://{address}"))?
        .connect()
        .await?;
    MachineRpcClient::new(channel)
        .list_machines(op::ListMachines::into_request(ListMachinesRequest {}).encode()?)
        .await?
        .into_inner()
        .decode_response()?
        .decode::<op::ListMachines>()?
        .enrollment
        .map(|enrollment| enrollment.target_versions)
        .ok_or(PeerError::NoEnrollment)
}

/// `container` as `machine` observes it, found in `machines` and asked over the
/// mesh within `bound`. The answer keeps the environment and Config content a
/// store row drops. `None` when `machines` does not hold the Machine or it
/// didn't answer.
pub(crate) async fn inspect_peer(
    machines: &ReplicatedObservations<Machine, MachineId>,
    machine: MachineId,
    container: ContainerId,
    bound: Duration,
) -> Option<ContainerDetails> {
    let peer = machines
        .observations
        .iter()
        .find(|peer| peer.id == machine)?;
    let address = SocketAddr::new(IpAddr::V6(peer.management_address().0), MACHINE_API_PORT);
    match tokio::time::timeout(bound, inspect_on(address, container)).await {
        Ok(Ok(copy)) => Some(copy),
        Ok(Err(error)) => {
            tracing::debug!(error = %ployz_core::error_chain::inline(&error), "could not inspect {container} on {}", peer.name);
            None
        }
        Err(_) => {
            tracing::debug!("timed out inspecting {container} on {}", peer.name);
            None
        }
    }
}

/// Like [`target_versions`], over the mesh with no iroh connection. The answer
/// keeps the environment and Config content.
async fn inspect_on(
    address: SocketAddr,
    container_id: ContainerId,
) -> Result<ContainerDetails, PeerError> {
    let channel = Endpoint::from_shared(format!("http://{address}"))?
        .connect()
        .await?;
    let request = InspectContainerRequest { container_id };
    Ok(MachineRpcClient::new(channel)
        .max_decoding_message_size(RUNTIME_WATCH_MESSAGE_SIZE_LIMIT)
        .inspect_container(op::InspectContainer::into_request(request).encode()?)
        .await?
        .into_inner()
        .decode_response()?
        .decode::<op::InspectContainer>()?)
}

/// `row`'s full spec as its owner `answer`s for it. `None` unless the answer
/// comes from the row's owner, which still reports the copy In at the gen the
/// row shows.
pub(crate) fn confirmed_row(
    row: &ContainerObservation,
    answer: ContainerObservation,
) -> Option<ResolvedServiceSpec> {
    (answer.machine_id == row.machine_id
        && *answer.rotation() == CopyRotation::In
        && answer.copy_gen() == row.copy_gen())
    .then(|| answer.into_parts().resolved_spec)
}

/// What a Deploy asked of a copy. The planner resolves each copy's update order on
/// its own: a replacement whose predecessor holds a host port stops first, while a
/// new copy starts first. So copies one Deploy started can differ there alone.
pub(crate) fn deploy_input(spec: &ResolvedServiceSpec) -> RequestedServiceSpec {
    let mut requested = spec.to_requested();
    requested.update.order = None;
    requested
}

/// Whether a Deploy of `wanted` would make a new Container in place of `current`.
pub(crate) fn recreates(wanted: &RequestedServiceSpec, current: &ResolvedServiceSpec) -> bool {
    compare_specs(current, wanted) == SpecChange::NeedsRecreate
}

/// A newer copy supersedes `current` when a Deploy of the newer copy's spec would
/// make a new Container in its place. A Deploy that only scales up keeps the older
/// copies, so they are not superseded. A replicated row has no environment or
/// Config content, so `current` is compared as published against one.
pub(crate) fn supersedes(newer: &ResolvedServiceSpec, current: &ResolvedServiceSpec) -> bool {
    recreates(&deploy_input(newer), current)
}
