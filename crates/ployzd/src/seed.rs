//! Seed a fresh core store from a mirrored `IntentSnapshot` on promotion.
//!
//! keeper cannot open the core store (it does not depend on ployzd), so
//! promotion drives this through a one-shot ployzd invocation: replay the
//! mirror's roster, routes, and serving targets into the fresh store, and set a
//! strictly higher epoch than the core being succeeded (ADR 0031). Every write
//! is an idempotent upsert and the epoch is max-then-bump, so re-running
//! `core-promote` is safe.

use crate::core_store::{CoreStore, CoreStoreError};
use crate::intent::machine_roster::MachineRosterStore;
use crate::intent::namespace_intent::NamespaceIntentStore;
use ployz_core::state::IntentSnapshot;

#[derive(Debug, thiserror::Error)]
pub enum SeedCoreError {
    #[error("seeding machine roster: {0}")]
    Roster(String),
    #[error("seeding namespace intent: {0}")]
    Namespace(String),
    #[error("seeding control-plane epoch: {0}")]
    Epoch(CoreStoreError),
}

/// Replay `snapshot` into `core_store` and bump the epoch past the succeeded core.
pub async fn seed_core_from_snapshot(
    core_store: &CoreStore,
    snapshot: &IntentSnapshot,
) -> Result<(), SeedCoreError> {
    let roster = MachineRosterStore::new(core_store.clone());
    let namespace = NamespaceIntentStore::new(core_store.clone());

    for machine in &snapshot.active_machines {
        roster
            .replace_active_machine(machine)
            .await
            .map_err(|error| SeedCoreError::Roster(error.to_string()))?;
    }
    for binding in &snapshot.route_bindings {
        namespace
            .replace_route_binding(binding.clone())
            .await
            .map_err(|error| SeedCoreError::Namespace(error.to_string()))?;
    }
    for entry in &snapshot.serving_target_entries {
        namespace
            .replace_serving_target_entry(entry.clone())
            .await
            .map_err(|error| SeedCoreError::Namespace(error.to_string()))?;
    }

    // Fence the succeeded core: advertise strictly above both the mirror's epoch
    // and any epoch this machine already held (a re-promotion).
    let existing = core_store
        .control_plane_epoch()
        .await
        .map_err(SeedCoreError::Epoch)?;
    let bumped = snapshot.epoch.max(existing).next();
    core_store
        .set_control_plane_epoch(bumped)
        .await
        .map_err(SeedCoreError::Epoch)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::state::{ActiveMachineState, ControlPlaneEpoch, MachineLifecycle};
    use ployz_test_support::ids::{machine_id, operation_id};

    fn snapshot_at_epoch(epoch: ControlPlaneEpoch) -> IntentSnapshot {
        IntentSnapshot {
            epoch,
            active_machines: vec![ActiveMachineState {
                machine_id: machine_id("machine_a"),
                name: ployz_core::machine::MachineName::try_new("machine_a").expect("name"),
                activated_by: operation_id("op_activate"),
                lifecycle: MachineLifecycle::Active,
                public_endpoint: None,
            }],
            route_bindings: Vec::new(),
            serving_target_entries: Vec::new(),
        }
    }

    #[tokio::test]
    async fn seeds_the_roster_and_fences_above_the_mirror_epoch() {
        let store = CoreStore::open_in_memory().await.expect("store");
        // Mirror is at epoch 3; a fresh store mints 1.
        let snapshot = snapshot_at_epoch(ControlPlaneEpoch::initial().next().next());

        seed_core_from_snapshot(&store, &snapshot)
            .await
            .expect("seed");

        let roster = MachineRosterStore::new(store.clone());
        assert_eq!(roster.active_machines().await.expect("roster").len(), 1);
        // max(mirror=3, existing=1) + 1 = 4 — strictly above the succeeded core.
        assert_eq!(store.control_plane_epoch().await.expect("epoch").get(), 4);
    }

    #[tokio::test]
    async fn re_seeding_never_lowers_the_epoch() {
        let store = CoreStore::open_in_memory().await.expect("store");
        seed_core_from_snapshot(&store, &snapshot_at_epoch(ControlPlaneEpoch::initial()))
            .await
            .expect("first seed");
        let after_first = store.control_plane_epoch().await.expect("epoch");
        // A second seed from the same (now-stale) mirror epoch still advances.
        seed_core_from_snapshot(&store, &snapshot_at_epoch(ControlPlaneEpoch::initial()))
            .await
            .expect("second seed");
        assert!(store.control_plane_epoch().await.expect("epoch") > after_first);
    }
}
