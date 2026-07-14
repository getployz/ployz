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
use crate::intent::certificate_intent::{CertificateIntentStore, CertificateIntentStoreError};
use crate::intent::lease_intent::{LeaseIntentStore, LeaseIntentStoreError};
use crate::intent::machine_roster::{MachineRosterStore, MachineRosterStoreError};
use crate::intent::namespace_intent::{NamespaceIntentStore, NamespaceIntentStoreError};
use crate::intent::nats_authorizations::{NatsAuthorizationStore, NatsAuthorizationStoreError};
use ployz_core::state::IntentSnapshot;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum SeedCoreError {
    #[error("seeding machine roster: {0}")]
    Roster(#[from] MachineRosterStoreError),
    #[error("seeding namespace intent: {0}")]
    Namespace(#[from] NamespaceIntentStoreError),
    #[error("seeding authorization grants: {0}")]
    Authorizations(#[from] NatsAuthorizationStoreError),
    #[error("seeding managed lease intent: {0}")]
    ManagedLease(#[from] LeaseIntentStoreError),
    #[error("seeding custom certificate intent: {0}")]
    Certificate(#[from] CertificateIntentStoreError),
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
    _certificate_state_dir: &Path,
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
    for pin in &snapshot.volume_pins {
        namespace.replace_volume_pin(pin.clone()).await?;
    }
    let lease_store = LeaseIntentStore::new(core_store.clone());
    match &snapshot.managed_lease {
        ployz_core::state::ManagedLeaseProjection::Ready { lease, bundle } => {
            lease_store
                .store_lease(lease.clone(), Some(bundle.clone()))
                .await?;
        }
        ployz_core::state::ManagedLeaseProjection::RecordOnly { lease } => {
            lease_store.restore_lease_record(lease.clone()).await?
        }
        ployz_core::state::ManagedLeaseProjection::Unacquired => {}
    }
    let certificate_store = CertificateIntentStore::new(core_store.clone());
    for active_cert in &snapshot.custom_certificates {
        certificate_store
            .seed_active_metadata(active_cert.clone())
            .await?;
    }

    // Reuse the succeeded core's grant set verbatim: the promoted core authorizes
    // the same operator, Cloud, and machine credentials, so nothing is locked out
    // (ADR 0031).
    let authorizations = NatsAuthorizationStore::new(core_store.clone());
    for grant in &snapshot.nats_authorizations {
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
    use ployz_core::cert::{
        ActiveCertState, AutoLeaseState, CertBundleRef, CertValidAt, CertValidityWindow,
        ManagedLeaseAcquireRequest, ManagedLeaseIntent,
    };
    use ployz_core::state::{ActiveMachineState, ControlPlaneEpoch, MachineLifecycle};
    use ployz_lease_worker::{LeaseWorkerRequest, LeaseWorkerResponse, StubLeaseWorker};
    use ployz_test_support::ids::{machine_id, operation_id};

    fn snapshot_at_epoch(epoch: ControlPlaneEpoch) -> IntentSnapshot {
        use ployz_core::nats_config::{
            CredentialGrant, CredentialName, CredentialRole, MintedNatsUser, NatsAuthorizationGrant,
        };
        IntentSnapshot {
            epoch,
            core_machine_id: machine_id("machine_a"),
            active_machines: vec![ActiveMachineState {
                machine_id: machine_id("machine_a"),
                name: ployz_core::machine::MachineName::try_new("machine_a").expect("name"),
                activated_by: operation_id("op_activate"),
                roles: ployz_core::roles::InstallRolePolicy::install_all(),
                lifecycle: MachineLifecycle::Active,
                control_endpoints: Vec::new(),
                mesh_endpoints: Vec::new(),
                endpoint_subnet: ployz_core::dataplane::MachineEndpointSubnet::try_new(
                    "10.198.0.0/24",
                )
                .expect("valid endpoint subnet"),
                wireguard_public_key: ployz_core::dataplane::WireGuardPublicKey::try_new(
                    "public-machine-a",
                )
                .expect("public key"),
            }],
            dataplane_projection: ployz_core::dataplane::DataplaneProjection::try_new(
                Vec::new(),
                None,
            )
            .expect("empty projection"),
            route_bindings: Vec::new(),
            serving_target_entries: Vec::new(),
            volume_pins: Vec::new(),
            nats_authorizations: vec![NatsAuthorizationGrant::Credential(CredentialGrant {
                public_key: MintedNatsUser::generate().expect("mint").public,
                name: CredentialName::try_new("Founder operator (machine_a)")
                    .expect("credential name"),
                role: CredentialRole::Operator,
            })],
            public_url_mode: ployz_core::cert::PublicUrlMode::Auto,
            managed_lease: ployz_core::state::ManagedLeaseProjection::Unacquired,
            custom_certificates: Vec::new(),
            acme_http01_challenges: Vec::new(),
        }
    }

    #[tokio::test]
    async fn seeds_the_roster_and_fences_above_the_mirror_epoch() {
        let store = CoreStore::open_in_memory().await.expect("store");
        // Mirror is at epoch 3; a fresh store mints 1.
        let snapshot = snapshot_at_epoch(ControlPlaneEpoch::initial().next().next());

        let certificates = tempfile::tempdir().expect("certificate state");
        let outcome = seed_core_from_snapshot(&store, &snapshot, certificates.path())
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
    async fn seeding_restores_ready_managed_lease_bundle() {
        let store = CoreStore::open_in_memory().await.expect("store");
        let mut worker = StubLeaseWorker::new();
        let LeaseWorkerResponse::LeaseAcquired(acquired) = worker
            .handle(LeaseWorkerRequest::Acquire(ManagedLeaseAcquireRequest {
                ipv4: Vec::new(),
                ipv6: Vec::new(),
            }))
            .expect("acquire fixture lease")
        else {
            panic!("acquire returns lease");
        };
        let pending = worker
            .handle(LeaseWorkerRequest::DownloadBundle {
                lease: acquired.lease.name.clone(),
                token: acquired.lease.token.clone(),
            })
            .expect("bundle pending");
        assert!(matches!(pending, LeaseWorkerResponse::BundlePending));
        let LeaseWorkerResponse::Bundle(bundle) = worker
            .handle(LeaseWorkerRequest::DownloadBundle {
                lease: acquired.lease.name.clone(),
                token: acquired.lease.token.clone(),
            })
            .expect("bundle ready")
        else {
            panic!("bundle ready");
        };
        let mut snapshot = snapshot_at_epoch(ControlPlaneEpoch::initial());
        snapshot.managed_lease = ployz_core::state::ManagedLeaseProjection::Ready {
            lease: acquired.lease.clone(),
            bundle: bundle.clone(),
        };

        let certificates = tempfile::tempdir().expect("certificate state");
        seed_core_from_snapshot(&store, &snapshot, certificates.path())
            .await
            .expect("seed ready lease");

        assert_eq!(
            LeaseIntentStore::new(store)
                .load()
                .await
                .expect("load lease"),
            ManagedLeaseIntent::Auto {
                state: Box::new(AutoLeaseState::Ready {
                    lease: acquired.lease,
                    bundle,
                }),
            }
        );
    }

    #[tokio::test]
    async fn seeding_restores_custom_certificate_metadata_without_secret_material() {
        let active = active_certificate();
        let mut snapshot = snapshot_at_epoch(ControlPlaneEpoch::initial());
        snapshot.custom_certificates = vec![active.clone()];
        let target_store = CoreStore::open_in_memory().await.expect("target store");
        let target_directory = tempfile::tempdir().expect("target certificate state");

        seed_core_from_snapshot(&target_store, &snapshot, target_directory.path())
            .await
            .expect("seed custom certificate");

        let seeded_certificates = CertificateIntentStore::new(target_store)
            .active_certificates()
            .await
            .expect("seeded certificates");
        let [seeded] = seeded_certificates.as_slice() else {
            panic!("one certificate is seeded");
        };
        assert_eq!(seeded, &active);
    }

    #[tokio::test]
    async fn re_running_on_an_advanced_store_is_a_no_op() {
        let store = CoreStore::open_in_memory().await.expect("store");
        let first = seed_core_from_snapshot(
            &store,
            &snapshot_at_epoch(ControlPlaneEpoch::initial()),
            tempfile::tempdir().expect("certificate state").path(),
        )
        .await
        .expect("first seed");
        assert_eq!(first, SeedOutcome::Seeded);
        let after_first = store.control_plane_epoch().await.expect("epoch");

        // The store now fences above the mirror; a stale re-run must not roll back.
        let second = seed_core_from_snapshot(
            &store,
            &snapshot_at_epoch(ControlPlaneEpoch::initial()),
            tempfile::tempdir().expect("certificate state").path(),
        )
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
            tempfile::tempdir().expect("certificate state").path(),
        )
        .await
        .expect("first seed");
        let after_first = store.control_plane_epoch().await.expect("epoch");

        // A mirror captured at the store's own generation (operator changes keep the
        // epoch, which is a promotion generation not an intent revision) must not
        // re-seed — that would replay stale intent under a higher epoch.
        let outcome = seed_core_from_snapshot(
            &store,
            &snapshot_at_epoch(after_first),
            tempfile::tempdir().expect("certificate state").path(),
        )
        .await
        .expect("same-epoch seed");
        assert_eq!(outcome, SeedOutcome::AlreadyPromoted);
        assert_eq!(
            store.control_plane_epoch().await.expect("epoch"),
            after_first
        );
    }

    fn active_certificate() -> ActiveCertState {
        ActiveCertState {
            cert_id: ployz_test_support::ids::cert_id("cert_app_example_com"),
            hostname: ployz_test_support::ids::route_hostname("app.example.com"),
            bundle_ref: CertBundleRef::try_new(format!(
                "sha256:{}:/var/lib/ployz/certificates/cert_app_example_com.bundle",
                "a".repeat(64)
            ))
            .expect("bundle ref"),
            validity: CertValidityWindow::try_new(
                CertValidAt::try_new(1).expect("not before"),
                CertValidAt::try_new(2).expect("not after"),
            )
            .expect("validity"),
        }
    }
}
