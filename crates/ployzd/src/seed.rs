//! Seed a fresh core store from a mirrored `IntentSnapshot` on promotion.
//!
//! Host Runner cannot open the core store (it does not depend on ployzd), so
//! promotion drives this through a one-shot ployzd invocation: replay the
//! mirror's roster, routes, and serving targets into the fresh store, and set a
//! strictly higher epoch than the core being succeeded (ADR 0031). Every write
//! is an idempotent upsert, and a store that has already served as a core is
//! rejected as a no-op, so re-running `core-promote` never rolls a live core's
//! intent back.

use crate::core_store::{CoreStore, CoreStoreError};
use crate::intent::machine_roster::{MachineRosterStore, MachineRosterStoreError};
use crate::intent::namespace_intent::{NamespaceIntentStore, NamespaceIntentStoreError};
use crate::intent::nats_authorizations::{NatsAuthorizationStore, NatsAuthorizationStoreError};
use ployz_core::state::IntentSnapshot;

#[derive(Debug, thiserror::Error)]
pub enum SeedCoreError {
    #[error("seeding machine roster: {0}")]
    Roster(#[from] MachineRosterStoreError),
    #[error("seeding namespace intent: {0}")]
    Namespace(#[from] NamespaceIntentStoreError),
    #[error("seeding authorization grants: {0}")]
    Authorizations(#[from] NatsAuthorizationStoreError),
    #[error("seeding control-plane epoch: {0}")]
    Epoch(#[from] CoreStoreError),
}

/// Whether a promotion seed replayed the mirror or found the store already ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedOutcome {
    /// The store was fresh: intent was replayed and the epoch fenced above the
    /// succeeded core.
    Seeded,
    /// The store has already served as a core (has an epoch row) — a live core
    /// whose intent must not be rolled back. Left untouched.
    AlreadyPromoted,
}

/// Replay `snapshot` into `core_store` and fence the epoch past the succeeded core.
pub async fn seed_core_from_snapshot(
    core_store: &CoreStore,
    snapshot: &IntentSnapshot,
) -> Result<SeedOutcome, SeedCoreError> {
    // Only a fresh store (never promoted) may be seeded. Once a store has an epoch
    // row it is a live core, and ControlPlaneEpoch is a promotion generation, not an
    // intent revision — post-promotion operator changes keep the same epoch, so
    // re-seeding from an older same-epoch mirror would roll that intent back under a
    // higher epoch machines accept as fresh. The roster is replayed before the epoch
    // is set, so a crashed partial seed (no epoch row yet) still re-runs.
    if core_store.control_plane_epoch_if_present().await?.is_some() {
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

    // Reuse the succeeded core's grant set verbatim: the promoted core authorizes
    // the same operator, Cloud, and machine credentials, so nothing is locked out
    // (ADR 0031).
    let authorizations = NatsAuthorizationStore::new(core_store.clone());
    for grant in &snapshot.authorized_users {
        authorizations.upsert(grant).await?;
    }

    // Fence the succeeded core in one atomic step, strictly above the mirror's
    // epoch (the store is fresh past the guard, so this lands at mirror.next()).
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
        use ployz_core::nats_config::{MintedNatsUser, NatsAuthorizedUser};
        use ployz_core::security::NatsPrincipal;
        IntentSnapshot {
            epoch,
            core_machine_id: machine_id("machine_a"),
            active_machines: vec![ActiveMachineState {
                machine_id: machine_id("machine_a"),
                name: ployz_core::machine::MachineName::try_new("machine_a").expect("name"),
                activated_by: operation_id("op_activate"),
                lifecycle: MachineLifecycle::Active,
                control_endpoints: Vec::new(),
                mesh_endpoints: Vec::new(),
            }],
            route_bindings: Vec::new(),
            serving_target_entries: Vec::new(),
            authorized_users: vec![NatsAuthorizedUser {
                principal: NatsPrincipal::Operator,
                nkey_public: MintedNatsUser::generate().expect("mint").public,
            }],
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
        // The grant set is reused verbatim so the promoted core locks no one out.
        let grants = NatsAuthorizationStore::new(store.clone());
        assert_eq!(grants.list().await.expect("grants").len(), 1);
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

    #[tokio::test]
    async fn re_running_with_a_same_epoch_mirror_does_not_roll_back() {
        let store = CoreStore::open_in_memory().await.expect("store");
        seed_core_from_snapshot(
            &store,
            &snapshot_at_epoch(ControlPlaneEpoch::initial().next()),
        )
        .await
        .expect("first seed");
        let after_first = store.control_plane_epoch().await.expect("epoch");

        // A mirror captured at the store's own generation (operator changes keep the
        // epoch, which is a promotion generation not an intent revision) must not
        // re-seed — that would replay stale intent under a higher epoch.
        let outcome = seed_core_from_snapshot(&store, &snapshot_at_epoch(after_first))
            .await
            .expect("same-epoch seed");
        assert_eq!(outcome, SeedOutcome::AlreadyPromoted);
        assert_eq!(
            store.control_plane_epoch().await.expect("epoch"),
            after_first
        );
    }
}
