//! Machine membership row adapters.
//!
//! This adapter owns Ployz product semantics. Polis supplies row statements,
//! endpoint identity, and peer probe primitives.

use std::time::Duration;

use crate::error::MachineFailure;
use crate::machine::{
    IrohEndpointId, MachineEpoch, MachineId, MachineMembership, MachineMembershipPort,
    MachineNetworkIdentity, MachineStatus, MachineTombstone, OverlayIp, WireGuardPublicKey,
};
use crate::operation::MutationContext;

pub(crate) struct CorrosionMachineMembership<P> {
    store: polis::CorrosionStore,
    probe: P,
    island: polis::IslandId,
}

impl<P> CorrosionMachineMembership<P> {
    #[must_use]
    pub(crate) fn new(store: polis::CorrosionStore, probe: P, island: polis::IslandId) -> Self {
        Self {
            store,
            probe,
            island,
        }
    }
}

pub(crate) fn start_corrosion_machine_membership<P>(
    store: polis::CorrosionStore,
    probe: P,
    island: polis::IslandId,
) -> impl MachineMembershipPort
where
    P: polis::PeerProbe,
{
    CorrosionMachineMembership::new(store, probe, island)
}

impl<P> MachineMembershipPort for CorrosionMachineMembership<P>
where
    P: polis::PeerProbe,
{
    async fn observe(
        &self,
        _context: &MutationContext,
        machine: &MachineId,
    ) -> Result<MachineStatus, MachineFailure> {
        let machine_id = polis_machine_id(machine)?;
        let Some(row) = observe_machine_row(
            &self.store,
            &self.island,
            &machine_id,
            polis::StoreTimeout::CONTROL_PLANE_DEFAULT,
        )
        .await
        .map_err(map_machine_store_error)?
        else {
            return Ok(MachineStatus::Absent);
        };

        machine_status_from_row(row)
    }

    async fn join(
        &self,
        _context: &MutationContext,
        membership: &MachineMembership,
    ) -> Result<MachineStatus, MachineFailure> {
        let endpoint = polis_endpoint_id(&membership.network.iroh_endpoint_id)?;
        self.probe
            .probe(
                &endpoint,
                polis::PeerProbeDeadline::new(Duration::from_secs(2)),
            )
            .await
            .map_err(|_| MachineFailure::PeerPreflightFailed {
                machine: membership.machine.clone(),
            })?;

        let row = machine_row_from_membership(membership, &self.island)?;
        upsert_machine_row(
            &self.store,
            &row,
            polis::StoreTimeout::CONTROL_PLANE_DEFAULT,
        )
        .await
        .map_err(map_machine_store_error)?;

        let machine_id = polis_machine_id(&membership.machine)?;
        let Some(row) = observe_machine_row(
            &self.store,
            &self.island,
            &machine_id,
            polis::StoreTimeout::CONTROL_PLANE_DEFAULT,
        )
        .await
        .map_err(map_machine_store_error)?
        else {
            return Ok(MachineStatus::Absent);
        };

        machine_status_from_row(row)
    }
}

async fn observe_machine_row(
    store: &polis::CorrosionStore,
    island_id: &polis::IslandId,
    machine_id: &polis::StoreMachineId,
    timeout: polis::StoreTimeout,
) -> Result<Option<polis::MachineRow>, polis::StoreError> {
    let query = polis::MachineRowQuery::by_island_machine_id(island_id, machine_id)?;
    let rows = store.query(query.statement(), timeout).await?;
    query.decode_optional(&rows)
}

async fn upsert_machine_row(
    store: &polis::CorrosionStore,
    row: &polis::MachineRow,
    timeout: polis::StoreTimeout,
) -> Result<(), polis::StoreError> {
    let statement = polis::upsert_machine_statement(row)?;
    store.execute_transaction(&[statement], timeout).await?;
    Ok(())
}

fn machine_row_from_membership(
    membership: &MachineMembership,
    island: &polis::IslandId,
) -> Result<polis::MachineRow, MachineFailure> {
    Ok(polis::MachineRow::new(
        polis_machine_id(&membership.machine)?,
        island.clone(),
        polis_endpoint_id(&membership.network.iroh_endpoint_id)?,
        polis::WireGuardPublicKey::parse(membership.network.wireguard_public_key.as_str())
            .map_err(map_machine_polis_error)?,
        polis::OverlayIp::parse(membership.network.overlay_ip.canonical())
            .map_err(map_machine_polis_error)?,
        polis::MembershipLifecycle::Active,
        polis::RowEpoch::new(membership.epoch.value()).map_err(map_machine_polis_error)?,
    ))
}

fn machine_status_from_row(row: polis::MachineRow) -> Result<MachineStatus, MachineFailure> {
    let membership = machine_membership_from_row(&row)?;
    match row.lifecycle() {
        polis::MembershipLifecycle::Active => Ok(MachineStatus::Joined(membership)),
        polis::MembershipLifecycle::Removing => Ok(MachineStatus::Removing {
            machine: membership.machine,
            epoch: membership.epoch,
        }),
        polis::MembershipLifecycle::Tombstoned | polis::MembershipLifecycle::Deleted => {
            Ok(MachineStatus::Tombstoned(MachineTombstone {
                machine: membership.machine,
                epoch: membership.epoch,
            }))
        }
        polis::MembershipLifecycle::Conflicted => Ok(MachineStatus::Conflicted {
            machine: membership.machine,
            epoch: membership.epoch,
        }),
    }
}

fn machine_membership_from_row(
    row: &polis::MachineRow,
) -> Result<MachineMembership, MachineFailure> {
    Ok(MachineMembership::new(
        MachineId::parse(row.machine_id().as_str())?,
        MachineEpoch::new(row.epoch().value())?,
        MachineNetworkIdentity::new(
            OverlayIp::parse(row.overlay_ip().as_str())?,
            IrohEndpointId::parse(row.endpoint_id().as_str())?,
            WireGuardPublicKey::parse(row.wireguard_public_key().as_str())?,
        ),
    ))
}

fn polis_machine_id(machine: &MachineId) -> Result<polis::StoreMachineId, MachineFailure> {
    polis::StoreMachineId::parse(machine.as_str()).map_err(map_machine_polis_error)
}

fn polis_endpoint_id(endpoint: &IrohEndpointId) -> Result<polis::IrohEndpointId, MachineFailure> {
    polis::IrohEndpointId::parse(endpoint.as_str()).map_err(map_machine_polis_error)
}

fn map_machine_store_error(error: polis::StoreError) -> MachineFailure {
    match error {
        polis::StoreError::MalformedPayload => MachineFailure::MembershipRowsPayloadInvalid,
        polis::StoreError::Timeout => MachineFailure::MembershipRowsTimeout,
        polis::StoreError::MissedChange { .. } => MachineFailure::MembershipRowsMissedChanges,
        polis::StoreError::Stream { .. } => MachineFailure::MembershipRowsStreamInterrupted,
        polis::StoreError::Client { .. }
        | polis::StoreError::Response { .. }
        | polis::StoreError::QueryChangedBeforeEndOfQuery
        | polis::StoreError::QueryEndedBeforeEndOfQuery => {
            MachineFailure::MembershipRowsUnavailable
        }
    }
}

fn map_machine_polis_error(error: polis::Error) -> MachineFailure {
    match error {
        polis::Error::MalformedPayload => MachineFailure::InvalidPayload,
        polis::Error::Unauthorized
        | polis::Error::Conflict
        | polis::Error::Timeout
        | polis::Error::StaleFence
        | polis::Error::NoResponder
        | polis::Error::FreshnessUnknown => MachineFailure::MembershipRowsUnavailable,
    }
}
