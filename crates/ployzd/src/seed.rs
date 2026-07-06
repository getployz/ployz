//! Seed a fresh core store from a mirrored `IntentSnapshot` on promotion.
//!
//! keeper cannot open the core store (it does not depend on ployzd), so
//! promotion drives this through a one-shot ployzd invocation: replay the
//! mirror's roster, routes, and serving targets into the fresh store, and set a
//! strictly higher epoch than the core being succeeded (ADR 0031). Every write
//! is an idempotent upsert, and a re-run whose mirror is already behind the
//! store's epoch is rejected as a no-op, so re-running `core-promote` never
//! rolls a live core's intent back.

use crate::core_store::{CoreStore, CoreStoreError};
use crate::intent::machine_roster::{MachineRosterStore, MachineRosterStoreError};
use crate::intent::namespace_intent::{NamespaceIntentStore, NamespaceIntentStoreError};
use ployz_core::state::IntentSnapshot;

#[derive(Debug, thiserror::Error)]
pub enum SeedCoreError {
    #[error("seeding machine roster: {0}")]
    Roster(#[from] MachineRosterStoreError),
    #[error("seeding namespace intent: {0}")]
    Namespace(#[from] NamespaceIntentStoreError),
    #[error("seeding control-plane epoch: {0}")]
    Epoch(#[from] CoreStoreError),
}

/// Whether a promotion seed replayed the mirror or found the store already ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedOutcome {
    /// The store was fresh (or behind the mirror): intent was replayed and the
    /// epoch fenced above the succeeded core.
    Seeded,
    /// The store already fenced a core above the mirror — a live core that took
    /// later operator changes. Left untouched; no rollback.
    AlreadyPromoted,
}

/// Replay `snapshot` into `core_store` and fence the epoch past the succeeded core.
pub async fn seed_core_from_snapshot(
    core_store: &CoreStore,
    snapshot: &IntentSnapshot,
) -> Result<SeedOutcome, SeedCoreError> {
    // Guard a stale re-run: if this store already fences a core above the mirror's
    // epoch, it is a live core that has taken later operator changes. Replaying the
    // stale snapshot would roll that intent back under a fresh epoch, and machines
    // would accept the rollback instead of epoch-gating it away. Leave it untouched.
    if core_store.control_plane_epoch().await? > snapshot.epoch {
        return Ok(SeedOutcome::AlreadyPromoted);
    }

    let roster = MachineRosterStore::new(core_store.clone());
    let namespace = NamespaceIntentStore::new(core_store.clone());

    for machine in &snapshot.active_machines {
        roster.replace_active_machine(machine).await?;
    }
    for binding in &snapshot.route_bindings {
        namespace.replace_route_binding(binding.clone()).await?;
    }
    for entry in &snapshot.serving_target_entries {
        namespace
            .replace_serving_target_entry(entry.clone())
            .await?;
    }

    // Fence the succeeded core in one atomic step, strictly above the mirror's
    // epoch (the guard bounds the store's existing epoch at <= the mirror).
    core_store
        .fence_control_plane_epoch_above(snapshot.epoch)
        .await?;
    Ok(SeedOutcome::Seeded)
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

        let outcome = seed_core_from_snapshot(&store, &snapshot)
            .await
            .expect("seed");

        assert_eq!(outcome, SeedOutcome::Seeded);
        let roster = MachineRosterStore::new(store.clone());
        assert_eq!(roster.active_machines().await.expect("roster").len(), 1);
        // max(mirror=3, existing=1) + 1 = 4 — strictly above the succeeded core.
        assert_eq!(store.control_plane_epoch().await.expect("epoch").get(), 4);
    }

    #[tokio::test]
    async fn re_running_on_an_advanced_store_is_a_no_op() {
        let store = CoreStore::open_in_memory().await.expect("store");
        let first =
            seed_core_from_snapshot(&store, &snapshot_at_epoch(ControlPlaneEpoch::initial()))
                .await
                .expect("first seed");
        assert_eq!(first, SeedOutcome::Seeded);
        let after_first = store.control_plane_epoch().await.expect("epoch");

        // The store now fences above the mirror; a stale re-run must not roll back.
        let second =
            seed_core_from_snapshot(&store, &snapshot_at_epoch(ControlPlaneEpoch::initial()))
                .await
                .expect("second seed");
        assert_eq!(second, SeedOutcome::AlreadyPromoted);
        assert_eq!(
            store.control_plane_epoch().await.expect("epoch"),
            after_first
        );
    }
}
