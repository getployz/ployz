//! Corrosion-row-backed machine membership adapter.
//!
//! This adapter owns Ployz product semantics. Polis supplies row statements,
//! endpoint identity, and peer probe primitives.

use std::{future::Future, time::Duration};

use crate::error::{MachineFailure, PrimitiveFailure};
use crate::machine::{
    IrohEndpointId, MachineEpoch, MachineId, MachineMembership, MachineMembershipPort,
    MachineNetworkIdentity, MachineRemoval, MachineRemovalReason, MachineStatus, MachineTombstone,
    OverlayIp, WireGuardPublicKey,
};
use crate::operation::MutationContext;

pub(crate) trait MachineMembershipRows {
    async fn observe_machine(
        &self,
        machine_id: &polis::StoreMachineId,
    ) -> Result<Option<polis::MachineRow>, polis::StoreError>;

    async fn upsert_machine(&self, row: &polis::MachineRow) -> Result<(), polis::StoreError>;
}

#[derive(Debug)]
pub(crate) struct CorrosionMachineMembership<R, P> {
    rows: R,
    probe: P,
    island: polis::IslandId,
}

impl<R, P> CorrosionMachineMembership<R, P> {
    #[must_use]
    pub(crate) fn new(rows: R, probe: P, island: polis::IslandId) -> Self {
        Self {
            rows,
            probe,
            island,
        }
    }
}

pub(crate) async fn start_corrosion_machine_membership<P>(
    store: polis::CorrosionStore,
    probe: P,
    island: polis::IslandId,
) -> Result<impl MachineMembershipPort, PrimitiveFailure>
where
    P: polis::PeerProbe,
{
    Ok(CorrosionMachineMembership::new(
        CorrosionMachineMembershipRows::start(store),
        probe,
        island,
    ))
}

impl<R, P> MachineMembershipPort for CorrosionMachineMembership<R, P>
where
    R: MachineMembershipRows,
    P: polis::PeerProbe,
{
    fn observe<'a>(
        &'a self,
        _context: &'a MutationContext,
        machine: &'a MachineId,
    ) -> impl Future<Output = Result<MachineStatus, MachineFailure>> + 'a {
        async move {
            let machine_id = polis_machine_id(machine)?;
            let Some(row) = self
                .rows
                .observe_machine(&machine_id)
                .await
                .map_err(map_machine_store_error)?
            else {
                return Ok(MachineStatus::Absent);
            };

            machine_status_from_row(row)
        }
    }

    fn join<'a>(
        &'a self,
        _context: &'a MutationContext,
        membership: &'a MachineMembership,
    ) -> impl Future<Output = Result<MachineStatus, MachineFailure>> + 'a {
        async move {
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
            self.rows
                .upsert_machine(&row)
                .await
                .map_err(map_machine_store_error)?;

            let machine_id = polis_machine_id(&membership.machine)?;
            let Some(row) = self
                .rows
                .observe_machine(&machine_id)
                .await
                .map_err(map_machine_store_error)?
            else {
                return Ok(MachineStatus::Absent);
            };

            machine_status_from_row(row)
        }
    }
}

struct CorrosionMachineMembershipRows {
    store: polis::CorrosionStore,
}

impl CorrosionMachineMembershipRows {
    fn start(store: polis::CorrosionStore) -> Self {
        Self { store }
    }
}

impl MachineMembershipRows for CorrosionMachineMembershipRows {
    async fn observe_machine(
        &self,
        machine_id: &polis::StoreMachineId,
    ) -> Result<Option<polis::MachineRow>, polis::StoreError> {
        observe_machine_row(
            &self.store,
            machine_id,
            polis::StoreTimeout::CONTROL_PLANE_DEFAULT,
        )
        .await
    }

    async fn upsert_machine(&self, row: &polis::MachineRow) -> Result<(), polis::StoreError> {
        upsert_machine_row(&self.store, row, polis::StoreTimeout::CONTROL_PLANE_DEFAULT).await
    }
}

async fn observe_machine_row(
    store: &polis::CorrosionStore,
    machine_id: &polis::StoreMachineId,
    timeout: polis::StoreTimeout,
) -> Result<Option<polis::MachineRow>, polis::StoreError> {
    let query = polis::MachineRowQuery::by_machine_id(machine_id)?;
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
        0,
    ))
}

fn machine_status_from_row(row: polis::MachineRow) -> Result<MachineStatus, MachineFailure> {
    let membership = machine_membership_from_row(&row)?;
    match row.lifecycle() {
        polis::MembershipLifecycle::Active => Ok(MachineStatus::Joined(membership)),
        polis::MembershipLifecycle::Removing => Ok(MachineStatus::Removing(MachineRemoval {
            machine: membership.machine,
            epoch: membership.epoch,
            reason: MachineRemovalReason::parse("corrosion-row")?,
        })),
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
        polis::StoreError::MalformedPayload => MachineFailure::ProjectionPayloadInvalid,
        polis::StoreError::Timeout => MachineFailure::ProjectionTimeout,
        polis::StoreError::MissedChange { .. } => MachineFailure::ProjectionMissedChanges,
        polis::StoreError::Stream { .. } => MachineFailure::ProjectionStreamInterrupted,
        polis::StoreError::Client { .. }
        | polis::StoreError::Response { .. }
        | polis::StoreError::QueryChangedBeforeEndOfQuery
        | polis::StoreError::QueryEndedBeforeEndOfQuery => MachineFailure::ProjectionUnavailable,
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
        | polis::Error::FreshnessUnknown
        | polis::Error::TerminalAlreadyWritten => MachineFailure::ProjectionUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::machine::{MachineAddOutcome, MachineAddRequest, MachineMembershipService};
    use crate::operation::{
        AuthorityContext, AuthorityEpoch, IdempotencyKey, OperationId, PrincipalId, ScopeId,
    };

    #[tokio::test]
    async fn absent_preflighted_machine_commits_one_membership_row() {
        let endpoint = polis::IrohEndpointId::parse("iroh-node-a").expect("endpoint");
        let rows = RecordingRows::default();
        let row_store = rows.clone();
        let probe = polis::FakePeerProbe::new().reachable(endpoint);
        let adapter = adapter(rows, probe);
        let service = MachineMembershipService::new(adapter);

        let outcome = service
            .add_machine(&context(), request("node-a", 1, "fd00::1"))
            .await
            .expect("add");

        assert!(
            matches!(outcome, MachineAddOutcome::Joined(joined) if joined.machine.as_str() == "node-a")
        );
        assert_eq!(row_store.committed.borrow().len(), 1);
        assert_eq!(row_store.committed.borrow().as_slice(), &["node-a"]);
    }

    #[tokio::test]
    async fn preflight_failure_writes_no_membership_row() {
        let endpoint = polis::IrohEndpointId::parse("iroh-node-a").expect("endpoint");
        let rows = RecordingRows::default();
        let probe = polis::FakePeerProbe::new().failing(endpoint, "dial failed");
        let adapter = adapter(rows, probe);
        let membership = membership("node-a", 1, "fd00::1");

        let error = adapter
            .join(&context(), &membership)
            .await
            .expect_err("preflight");

        assert_eq!(
            error,
            MachineFailure::PeerPreflightFailed {
                machine: MachineId::parse("node-a").expect("machine")
            }
        );
        assert!(adapter.rows.committed.borrow().is_empty());
    }

    #[tokio::test]
    async fn existing_active_row_observes_as_joined() {
        let rows = RecordingRows::default().with_row(row("node-a", 1, "fd00::1"));
        let probe = polis::FakePeerProbe::new();
        let adapter = adapter(rows, probe);

        let observed = adapter
            .observe(&context(), &MachineId::parse("node-a").expect("machine"))
            .await
            .expect("observe");

        assert!(
            matches!(observed, MachineStatus::Joined(joined) if joined == membership("node-a", 1, "fd00::1"))
        );
    }

    #[tokio::test]
    async fn join_returns_existing_active_row_after_protected_upsert_noop() {
        let rows = RecordingRows::default().with_row(row("node-a", 2, "fd00::2"));
        let probe = reachable_probe("node-a");
        let adapter = adapter(rows, probe);

        let observed = adapter
            .join(&context(), &membership("node-a", 1, "fd00::1"))
            .await
            .expect("join");

        assert!(
            matches!(observed, MachineStatus::Joined(joined) if joined == membership("node-a", 2, "fd00::2"))
        );
    }

    #[tokio::test]
    async fn join_returns_existing_removing_row_after_protected_upsert_noop() {
        let rows = RecordingRows::default().with_row(row_with_lifecycle(
            "node-a",
            2,
            "fd00::2",
            polis::MembershipLifecycle::Removing,
        ));
        let probe = reachable_probe("node-a");
        let adapter = adapter(rows, probe);

        let observed = adapter
            .join(&context(), &membership("node-a", 1, "fd00::1"))
            .await
            .expect("join");

        assert!(matches!(observed, MachineStatus::Removing(removal)
                if removal.machine.as_str() == "node-a" && removal.epoch.value() == 2));
    }

    #[tokio::test]
    async fn join_returns_existing_tombstone_after_protected_upsert_noop() {
        let rows = RecordingRows::default().with_row(row_with_lifecycle(
            "node-a",
            2,
            "fd00::2",
            polis::MembershipLifecycle::Tombstoned,
        ));
        let probe = reachable_probe("node-a");
        let adapter = adapter(rows, probe);

        let observed = adapter
            .join(&context(), &membership("node-a", 1, "fd00::1"))
            .await
            .expect("join");

        assert!(matches!(observed, MachineStatus::Tombstoned(tombstone)
                if tombstone.machine.as_str() == "node-a" && tombstone.epoch.value() == 2));
    }

    fn adapter(
        rows: RecordingRows,
        probe: polis::FakePeerProbe,
    ) -> CorrosionMachineMembership<RecordingRows, polis::FakePeerProbe> {
        CorrosionMachineMembership::new(
            rows,
            probe,
            polis::IslandId::parse("prod").expect("island"),
        )
    }

    #[derive(Clone, Default, Debug)]
    struct RecordingRows {
        row: Rc<RefCell<Option<polis::MachineRow>>>,
        committed: Rc<RefCell<Vec<String>>>,
    }

    impl RecordingRows {
        fn with_row(self, row: polis::MachineRow) -> Self {
            self.row.replace(Some(row));
            self
        }
    }

    impl MachineMembershipRows for RecordingRows {
        async fn observe_machine(
            &self,
            _machine_id: &polis::StoreMachineId,
        ) -> Result<Option<polis::MachineRow>, polis::StoreError> {
            Ok(self.row.borrow().clone())
        }

        async fn upsert_machine(&self, row: &polis::MachineRow) -> Result<(), polis::StoreError> {
            let mut stored = self.row.borrow_mut();
            if stored.as_ref().is_none_or(|current| current == row) {
                *stored = Some(row.clone());
            }
            self.committed
                .borrow_mut()
                .push(row.machine_id().as_str().to_string());
            Ok(())
        }
    }

    fn context() -> MutationContext {
        MutationContext::new(
            OperationId::parse("machine-add-1").expect("operation"),
            IdempotencyKey::parse("machine-add-1").expect("idempotency"),
            AuthorityContext::new(
                PrincipalId::parse("operator").expect("principal"),
                ScopeId::parse("prod").expect("scope"),
                AuthorityEpoch::new(7),
            ),
            None,
            UNIX_EPOCH + Duration::from_secs(60),
        )
    }

    fn reachable_probe(machine: &str) -> polis::FakePeerProbe {
        polis::FakePeerProbe::new()
            .reachable(polis::IrohEndpointId::parse(format!("iroh-{machine}")).expect("endpoint"))
    }

    fn request(machine: &str, epoch: u64, overlay_ip: &str) -> MachineAddRequest {
        MachineAddRequest {
            machine: MachineId::parse(machine).expect("machine"),
            epoch: MachineEpoch::new(epoch).expect("epoch"),
            network: network(machine, overlay_ip),
        }
    }

    fn membership(machine: &str, epoch: u64, overlay_ip: &str) -> MachineMembership {
        MachineMembership::new(
            MachineId::parse(machine).expect("machine"),
            MachineEpoch::new(epoch).expect("epoch"),
            network(machine, overlay_ip),
        )
    }

    fn network(machine: &str, overlay_ip: &str) -> MachineNetworkIdentity {
        MachineNetworkIdentity::new(
            OverlayIp::parse(overlay_ip).expect("overlay ip"),
            IrohEndpointId::parse(format!("iroh-{machine}")).expect("endpoint"),
            WireGuardPublicKey::parse(format!("wg-{machine}")).expect("wireguard"),
        )
    }

    fn row(machine: &str, epoch: u64, overlay_ip: &str) -> polis::MachineRow {
        row_with_lifecycle(
            machine,
            epoch,
            overlay_ip,
            polis::MembershipLifecycle::Active,
        )
    }

    fn row_with_lifecycle(
        machine: &str,
        epoch: u64,
        overlay_ip: &str,
        lifecycle: polis::MembershipLifecycle,
    ) -> polis::MachineRow {
        polis::MachineRow::new(
            polis::StoreMachineId::parse(machine).expect("machine"),
            polis::IslandId::parse("prod").expect("island"),
            polis::IrohEndpointId::parse(format!("iroh-{machine}")).expect("endpoint"),
            polis::WireGuardPublicKey::parse(format!("wg-{machine}")).expect("wireguard"),
            polis::OverlayIp::parse(overlay_ip).expect("overlay"),
            lifecycle,
            polis::RowEpoch::new(epoch).expect("epoch"),
            0,
        )
    }
}
