//! Corrosion-row-backed machine membership adapter.
//!
//! This adapter owns Ployz product semantics. Polis supplies row statements,
//! endpoint identity, and peer probe primitives.

use std::time::Duration;

use crate::error::MachineFailure;
use crate::machine::{
    IrohEndpointId, MachineEpoch, MachineId, MachineMembership, MachineMembershipPort,
    MachineNetworkIdentity, MachineRemoval, MachineRemovalReason, MachineStatus, MachineTombstone,
    MachineWireGuardPeer, MachineWireGuardPeerPort, OverlayIp, WireGuardPublicKey,
};
use crate::operation::MutationContext;

pub(crate) trait MachineMembershipRows {
    fn observe_machine(
        &self,
        machine_id: &polis::StoreMachineId,
    ) -> Result<Option<polis::MachineRow>, polis::StoreError>;

    fn upsert_machine(&self, row: &polis::MachineRow) -> Result<(), polis::StoreError>;

    fn wireguard_peers_for_machine(
        &self,
        machine_id: &polis::StoreMachineId,
    ) -> Result<Vec<polis::MachineRow>, polis::StoreError>;
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

impl<R, P> MachineWireGuardPeerPort for CorrosionMachineMembership<R, P>
where
    R: MachineMembershipRows,
{
    fn wireguard_peers(
        &self,
        _context: &MutationContext,
        machine: &MachineId,
    ) -> Result<Vec<MachineWireGuardPeer>, MachineFailure> {
        let machine_id = polis_machine_id(machine)?;
        self.rows
            .wireguard_peers_for_machine(&machine_id)
            .map_err(map_machine_store_error)?
            .into_iter()
            .map(|row| machine_wireguard_peer_from_row(&row))
            .collect()
    }
}

impl<R, P> MachineMembershipPort for CorrosionMachineMembership<R, P>
where
    R: MachineMembershipRows,
    P: polis::PeerProbe,
{
    fn observe(
        &self,
        _context: &MutationContext,
        machine: &MachineId,
    ) -> Result<MachineStatus, MachineFailure> {
        let machine_id = polis_machine_id(machine)?;
        let Some(row) = self
            .rows
            .observe_machine(&machine_id)
            .map_err(map_machine_store_error)?
        else {
            return Ok(MachineStatus::Absent);
        };

        machine_status_from_row(row)
    }

    fn join(
        &self,
        _context: &MutationContext,
        membership: &MachineMembership,
    ) -> Result<MachineMembership, MachineFailure> {
        let endpoint = polis_endpoint_id(&membership.network.iroh_endpoint_id)?;
        self.probe
            .probe(
                &endpoint,
                polis::PeerProbeDeadline::new(Duration::from_secs(2)),
            )
            .map_err(|_| MachineFailure::PeerPreflightFailed {
                machine: membership.machine.clone(),
            })?;

        let row = machine_row_from_membership(membership, &self.island)?;
        self.rows
            .upsert_machine(&row)
            .map_err(map_machine_store_error)?;

        let machine_id = polis_machine_id(&membership.machine)?;
        match self
            .rows
            .observe_machine(&machine_id)
            .map_err(map_machine_store_error)?
        {
            Some(row) => match machine_status_from_row(row)? {
                MachineStatus::Joined(joined) if joined == *membership => Ok(joined),
                MachineStatus::Joined(joined) => Err(MachineFailure::MembershipConflict {
                    machine: joined.machine,
                    epoch: Some(joined.epoch),
                }),
                MachineStatus::Conflicted { machine, epoch } => {
                    Err(MachineFailure::MembershipConflict {
                        machine,
                        epoch: Some(epoch),
                    })
                }
                MachineStatus::Removing(removal) => Err(MachineFailure::MachineRemoving {
                    machine: removal.machine,
                    epoch: removal.epoch,
                }),
                MachineStatus::Tombstoned(tombstone) => Err(MachineFailure::MachineTombstoned {
                    machine: tombstone.machine,
                    epoch: tombstone.epoch,
                }),
                MachineStatus::Absent => Err(MachineFailure::MutationMismatch {
                    machine: membership.machine.clone(),
                }),
            },
            None => Err(MachineFailure::MutationMismatch {
                machine: membership.machine.clone(),
            }),
        }
    }
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
        polis::CapabilitiesJson::parse("{}").map_err(map_machine_polis_error)?,
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

fn machine_wireguard_peer_from_row(
    row: &polis::MachineRow,
) -> Result<MachineWireGuardPeer, MachineFailure> {
    Ok(MachineWireGuardPeer::new(
        MachineId::parse(row.machine_id().as_str())?,
        OverlayIp::parse(row.overlay_ip().as_str())?,
        WireGuardPublicKey::parse(row.wireguard_public_key().as_str())?,
    ))
}

fn polis_machine_id(machine: &MachineId) -> Result<polis::StoreMachineId, MachineFailure> {
    polis::StoreMachineId::parse(machine.as_str()).map_err(map_machine_polis_error)
}

fn polis_endpoint_id(endpoint: &IrohEndpointId) -> Result<polis::IrohEndpointId, MachineFailure> {
    polis::IrohEndpointId::parse(endpoint.as_str()).map_err(map_machine_polis_error)
}

fn map_machine_store_error(_error: polis::StoreError) -> MachineFailure {
    MachineFailure::ProjectionUnavailable
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

    #[test]
    fn absent_preflighted_machine_commits_one_membership_row() {
        let endpoint = polis::IrohEndpointId::parse("iroh-node-a").expect("endpoint");
        let rows = RecordingRows::default();
        let row_store = rows.clone();
        let probe = polis::FakePeerProbe::new().reachable(endpoint);
        let adapter = adapter(rows, probe);
        let service = MachineMembershipService::new(adapter);

        let outcome = service
            .add_machine(&context(), request("node-a", 1, "fd00::1"))
            .expect("add");

        assert!(
            matches!(outcome, MachineAddOutcome::Joined(joined) if joined.machine.as_str() == "node-a")
        );
        assert_eq!(row_store.committed.borrow().len(), 1);
        assert_eq!(row_store.committed.borrow().as_slice(), &["node-a"]);
    }

    #[test]
    fn preflight_failure_writes_no_membership_row() {
        let endpoint = polis::IrohEndpointId::parse("iroh-node-a").expect("endpoint");
        let rows = RecordingRows::default();
        let probe = polis::FakePeerProbe::new().failing(endpoint, "dial failed");
        let adapter = adapter(rows, probe);
        let membership = membership("node-a", 1, "fd00::1");

        let error = adapter
            .join(&context(), &membership)
            .expect_err("preflight");

        assert_eq!(
            error,
            MachineFailure::PeerPreflightFailed {
                machine: MachineId::parse("node-a").expect("machine")
            }
        );
        assert!(adapter.rows.committed.borrow().is_empty());
    }

    #[test]
    fn existing_active_row_observes_as_joined() {
        let rows = RecordingRows::default().with_row(row("node-a", 1, "fd00::1"));
        let probe = polis::FakePeerProbe::new();
        let adapter = adapter(rows, probe);

        let observed = adapter
            .observe(&context(), &MachineId::parse("node-a").expect("machine"))
            .expect("observe");

        assert!(
            matches!(observed, MachineStatus::Joined(joined) if joined == membership("node-a", 1, "fd00::1"))
        );
    }

    #[test]
    fn namespace_derived_wireguard_peers_are_decoded_as_machine_memberships() {
        let peer = row("node-b", 1, "fd00::2");
        let rows = RecordingRows::default().with_wireguard_peers(vec![peer]);
        let probe = polis::FakePeerProbe::new();
        let adapter = adapter(rows, probe);

        let peers = adapter
            .wireguard_peers(&context(), &MachineId::parse("node-a").expect("machine"))
            .expect("peers");

        assert_eq!(
            peers,
            vec![MachineWireGuardPeer::new(
                MachineId::parse("node-b").expect("machine"),
                OverlayIp::parse("fd00::2").expect("overlay"),
                WireGuardPublicKey::parse("wg-node-b").expect("wireguard"),
            )]
        );
    }

    #[test]
    fn stale_upsert_reobserves_existing_joined_row_as_conflict() {
        let endpoint = polis::IrohEndpointId::parse("iroh-node-a").expect("endpoint");
        let rows = RecordingRows::default()
            .with_row(row("node-a", 2, "fd00::2"))
            .ignoring_upserts();
        let probe = polis::FakePeerProbe::new().reachable(endpoint);
        let adapter = adapter(rows, probe);

        let error = adapter
            .join(&context(), &membership("node-a", 1, "fd00::1"))
            .expect_err("conflict");

        assert!(
            matches!(error, MachineFailure::MembershipConflict { machine, epoch: Some(epoch) }
                if machine.as_str() == "node-a" && epoch.value() == 2)
        );
    }

    #[test]
    fn stale_upsert_reobserves_conflicted_row_as_conflict() {
        let endpoint = polis::IrohEndpointId::parse("iroh-node-a").expect("endpoint");
        let rows = RecordingRows::default()
            .with_row(row_with_lifecycle(
                "node-a",
                2,
                "fd00::2",
                polis::MembershipLifecycle::Conflicted,
            ))
            .ignoring_upserts();
        let probe = polis::FakePeerProbe::new().reachable(endpoint);
        let adapter = adapter(rows, probe);

        let error = adapter
            .join(&context(), &membership("node-a", 1, "fd00::1"))
            .expect_err("conflict");

        assert!(
            matches!(error, MachineFailure::MembershipConflict { machine, epoch: Some(epoch) }
                if machine.as_str() == "node-a" && epoch.value() == 2)
        );
    }

    #[test]
    fn stale_upsert_reobserves_removing_row_as_removing() {
        let endpoint = polis::IrohEndpointId::parse("iroh-node-a").expect("endpoint");
        let rows = RecordingRows::default()
            .with_row(row_with_lifecycle(
                "node-a",
                2,
                "fd00::2",
                polis::MembershipLifecycle::Removing,
            ))
            .ignoring_upserts();
        let probe = polis::FakePeerProbe::new().reachable(endpoint);
        let adapter = adapter(rows, probe);

        let error = adapter
            .join(&context(), &membership("node-a", 1, "fd00::1"))
            .expect_err("removing");

        assert!(
            matches!(error, MachineFailure::MachineRemoving { machine, epoch }
                if machine.as_str() == "node-a" && epoch.value() == 2)
        );
    }

    #[test]
    fn stale_upsert_reobserves_deleted_row_as_tombstoned() {
        let endpoint = polis::IrohEndpointId::parse("iroh-node-a").expect("endpoint");
        let rows = RecordingRows::default()
            .with_row(row_with_lifecycle(
                "node-a",
                2,
                "fd00::2",
                polis::MembershipLifecycle::Deleted,
            ))
            .ignoring_upserts();
        let probe = polis::FakePeerProbe::new().reachable(endpoint);
        let adapter = adapter(rows, probe);

        let error = adapter
            .join(&context(), &membership("node-a", 1, "fd00::1"))
            .expect_err("tombstoned");

        assert!(
            matches!(error, MachineFailure::MachineTombstoned { machine, epoch }
                if machine.as_str() == "node-a" && epoch.value() == 2)
        );
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
        wireguard_peers: Rc<RefCell<Vec<polis::MachineRow>>>,
        ignore_upserts: bool,
    }

    impl RecordingRows {
        fn with_row(self, row: polis::MachineRow) -> Self {
            self.row.replace(Some(row));
            self
        }

        fn ignoring_upserts(mut self) -> Self {
            self.ignore_upserts = true;
            self
        }

        fn with_wireguard_peers(self, peers: Vec<polis::MachineRow>) -> Self {
            self.wireguard_peers.replace(peers);
            self
        }
    }

    impl MachineMembershipRows for RecordingRows {
        fn observe_machine(
            &self,
            _machine_id: &polis::StoreMachineId,
        ) -> Result<Option<polis::MachineRow>, polis::StoreError> {
            Ok(self.row.borrow().clone())
        }

        fn upsert_machine(&self, row: &polis::MachineRow) -> Result<(), polis::StoreError> {
            if !self.ignore_upserts {
                self.row.replace(Some(row.clone()));
            }
            self.committed
                .borrow_mut()
                .push(row.machine_id().as_str().to_string());
            Ok(())
        }

        fn wireguard_peers_for_machine(
            &self,
            _machine_id: &polis::StoreMachineId,
        ) -> Result<Vec<polis::MachineRow>, polis::StoreError> {
            Ok(self.wireguard_peers.borrow().clone())
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
            polis::CapabilitiesJson::parse("{}").expect("capabilities"),
            lifecycle,
            polis::RowEpoch::new(epoch).expect("epoch"),
            0,
        )
    }
}
