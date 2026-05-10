use super::execute::{
    ParticipantSet, apply_with_certificate_coordination, apply_with_initial_plan,
    ensure_plan_stable, run_phase_startup,
};
use super::lifecycle::PreparedDeploy;
use super::plan::{deployable_machines, desired_slots, resolve_plan};
use super::preview;
use super::probe::{NoopParticipantProbe, ParticipantProbe, ProbeError, ProbeErrorKind};
use crate::certificates::{
    LocalHttp01ChallengeReadiness, NoopAcmeAccountCoordinator, NoopAcmeIssuerFactory,
    NoopIssuanceCoordinator,
};
use crate::deploy::participant::{
    CleanupVolumeCloneRequest, CloneVolumeRequest, CloneVolumeResult, DeployParticipantClient,
    MoveVolumeRequest, MoveVolumeResult, StartCandidateRequest,
};
use crate::error::{DeployError, Error, Result};
use crate::model::RegionRole;
use crate::model::{
    AcmeAccountRecord, AcmeChallengeReadinessRecord, AcmeChallengeRecord, CertificateRecord,
    DeployChangeKind, DeployId, DeployPhaseAdvancePolicy, DeployPhaseCommitPolicy, DeployPhaseId,
    DeployPhaseRecord, DeployPhaseRollbackPolicy, DeployPhaseState, DeployPhaseWork, DeployRecord,
    DeployState, DrainState, InstanceId, InstancePhase, InstanceStatusRecord, MachineId,
    MachineLifecycle, MachineMembership, MachineTopology, OverlayIp, PublicKey,
    ServiceBranchLineageRecord, ServiceRelease, ServiceReleaseRecord, ServiceReleaseSlot,
    ServiceRevisionRecord, ServiceRoutingPolicy, SlotId, VolumeBranchLineageRecord,
    VolumeMovementRecord, VolumeRecord,
};
use async_trait::async_trait;
use ployz_store_api::memory::{MemoryService, MemoryStore};
use ployz_store_api::{
    CertificateStore, DeployCommit, DeployStore, ImageAvailabilityStore, InstanceStatusStore,
    InviteStore, MachineMembershipStore, MachineSubscription, PeerRttStore,
    RoutingEventSubscription, RoutingStateStore, StoreDriver, StoreRuntimeControl, SyncProbe,
};
use ployz_types::Result as PloyzResult;
use ployz_types::spec::{
    ContainerSpec, DeployIntent, DeployManifest, DeployPhaseIntent, HttpRoute, Mount, MountSource,
    Namespace, NetworkMode, Placement, PortProtocol, PullPolicy, Resources, RestartPolicy,
    RolloutStrategy, RouteSpec, ServiceIntent, ServiceIntentHint, ServicePort, ServiceSpec,
    VolumeCloneConsistency, VolumeCloneDataPolicy, VolumeDeclaration, VolumeIntent,
    VolumeIntentHint, VolumeScope,
};
use std::collections::{BTreeMap, HashMap};
use std::net::Ipv6Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::sleep;

#[async_trait]
trait TestStoreSeed {
    async fn upsert_service_release(&self, record: &ServiceReleaseRecord) -> PloyzResult<()>;
    async fn list_service_releases(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<ServiceReleaseRecord>>;
}

#[async_trait]
impl TestStoreSeed for StoreDriver {
    async fn upsert_service_release(&self, record: &ServiceReleaseRecord) -> PloyzResult<()> {
        self.commit_deploy(&DeployCommit {
            namespace: record.namespace.clone(),
            revisions: Vec::new(),
            removed_services: Vec::new(),
            removed_volumes: Vec::new(),
            branch_lineage: Vec::new(),
            volume_movements: Vec::new(),
            volume_branches: Vec::new(),
            phase_commits: Vec::new(),
            releases: vec![record.clone()],
            volumes: Vec::new(),
            deploy: test_deploy_record(&record.namespace, "seed-deploy"),
        })
        .await
    }

    async fn list_service_releases(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<ServiceReleaseRecord>> {
        self.list_deploy_releases(namespace).await
    }
}

#[test]
fn deployable_machines_filters_by_participation() {
    let machines = vec![
        test_machine("enabled-a", MachineLifecycle::Active),
        test_machine("enabled-b", MachineLifecycle::Active),
        test_machine("draining", MachineLifecycle::Draining),
    ];

    let deployable = deployable_machines(&machines, &MachineId("local".into()));
    assert_eq!(
        deployable,
        vec![MachineId("enabled-a".into()), MachineId("enabled-b".into())]
    );
}

#[test]
fn deployable_machines_returns_empty_when_stored_machines_are_not_eligible() {
    let machines = vec![test_machine("draining", MachineLifecycle::Draining)];

    let deployable = deployable_machines(&machines, &MachineId("local".into()));
    assert!(deployable.is_empty());
}

#[test]
fn deployable_machines_falls_back_to_local_when_inventory_is_empty() {
    let deployable = deployable_machines(&[], &MachineId("local".into()));
    assert_eq!(deployable, vec![MachineId("local".into())]);
}

#[test]
fn deployable_machines_includes_compute_region_and_excludes_draining_regions() {
    let machines = vec![
        test_machine_in_region("home", MachineLifecycle::Active, RegionRole::HomeData),
        test_machine_in_region("compute", MachineLifecycle::Active, RegionRole::Compute),
        test_machine_in_region(
            "region-draining",
            MachineLifecycle::Active,
            RegionRole::Draining,
        ),
        test_machine_in_region(
            "region-disabled",
            MachineLifecycle::Active,
            RegionRole::Disabled,
        ),
    ];

    let deployable = deployable_machines(&machines, &MachineId("local".into()));
    assert_eq!(
        deployable,
        vec![MachineId("compute".into()), MachineId("home".into())]
    );
}

#[tokio::test]
async fn resolve_plan_preview_includes_default_phase_for_basic_manifest() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:latest",
    )]);

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");
    let preview = plan.to_preview(Vec::new());

    let [phase] = preview.phases.as_slice() else {
        panic!("expected one default deploy phase");
    };
    assert_eq!(phase.phase_id, DeployPhaseId("deploy".into()));
    assert_eq!(phase.name, "Deploy");
    assert_eq!(phase.order, 0);
    assert_eq!(phase.participants, vec![MachineId("machine-a".into())]);
    assert_eq!(phase.commit_policy, DeployPhaseCommitPolicy::EndOfDeploy);
    assert_eq!(phase.rollback_policy, DeployPhaseRollbackPolicy::Reversible);
    assert_eq!(phase.advance_policy, DeployPhaseAdvancePolicy::Immediate);
    assert!(matches!(
        phase.work.as_slice(),
        [DeployPhaseWork::Service { service, action }]
            if service == "api" && *action == DeployChangeKind::Create
    ));
}

#[tokio::test]
async fn resolve_plan_rejects_manifest_phase_named_deploy() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = test_manifest(vec![
        test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17"),
        test_service_spec("web", Placement::Replicated { count: 1 }, "nginx:1.27"),
    ]);
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: Vec::new(),
        phases: vec![DeployPhaseIntent {
            phase_id: "deploy".into(),
            name: Some("Database".into()),
            after: Vec::new(),
            services: vec!["db".into()],
            volumes: Vec::new(),
            commit_policy: DeployPhaseCommitPolicy::Checkpoint,
            rollback_policy: DeployPhaseRollbackPolicy::ForwardOnly,
            advance_policy: DeployPhaseAdvancePolicy::Immediate,
        }],
    });

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("reserved phase id should be rejected");

    assert!(
        error.to_string().contains("reserved"),
        "expected reserved phase id error, got {error}"
    );
}

#[tokio::test]
async fn resolve_plan_rejects_manual_phase_advance_policy() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::Replicated { count: 1 },
        "nginx:1.27",
    )]);
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: Vec::new(),
        phases: vec![DeployPhaseIntent {
            phase_id: "web".into(),
            name: Some("Web".into()),
            after: Vec::new(),
            services: vec!["web".into()],
            volumes: Vec::new(),
            commit_policy: DeployPhaseCommitPolicy::Checkpoint,
            rollback_policy: DeployPhaseRollbackPolicy::ForwardOnly,
            advance_policy: DeployPhaseAdvancePolicy::Manual,
        }],
    });

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("manual phase advancement should be rejected before execution");

    assert!(
        error.to_string().contains("advance policy Manual"),
        "expected manual advance policy error, got {error}"
    );
}

#[test]
fn replicated_one_reuses_existing_slot_machine() {
    let spec = test_service_spec("api", Placement::Replicated { count: 1 }, "nginx:latest");
    let machines = vec![MachineId("machine-a".into()), MachineId("machine-b".into())];
    let current_slots = [ServiceReleaseSlot {
        slot_id: SlotId("slot-0001".into()),
        machine_id: MachineId("machine-b".into()),
        active_instance_id: InstanceId("inst-1".into()),
        revision_hash: "rev-1".into(),
    }];

    let machine_map = HashMap::from([
        (
            MachineId("machine-a".into()),
            test_machine("machine-a", MachineLifecycle::Active),
        ),
        (
            MachineId("machine-b".into()),
            test_machine("machine-b", MachineLifecycle::Active),
        ),
    ]);

    let desired = desired_slots(
        &spec,
        &machines,
        Some(&current_slots),
        &machine_map,
        None,
        "rev-1",
        false,
    )
    .expect("desired slots");
    let [slot] = desired.as_slice() else {
        panic!("expected one desired slot");
    };
    assert_eq!(slot.slot_id, SlotId("slot-0001".into()));
    assert_eq!(slot.machine_id, MachineId("machine-b".into()));
}

#[test]
fn replicated_slot_relocates_from_draining_machine_during_deploy() {
    let spec = test_service_spec("api", Placement::Replicated { count: 2 }, "nginx:latest");
    let machines = vec![MachineId("machine-a".into())];
    let current_slots = [ServiceReleaseSlot {
        slot_id: SlotId("slot-0001".into()),
        machine_id: MachineId("machine-b".into()),
        active_instance_id: InstanceId("inst-1".into()),
        revision_hash: "rev-1".into(),
    }];

    let machine_map = HashMap::from([
        (
            MachineId("machine-a".into()),
            test_machine("machine-a", MachineLifecycle::Active),
        ),
        (
            MachineId("machine-b".into()),
            test_machine("machine-b", MachineLifecycle::Draining),
        ),
    ]);

    let desired = desired_slots(
        &spec,
        &machines,
        Some(&current_slots),
        &machine_map,
        None,
        "rev-1",
        false,
    )
    .expect("desired slots");

    assert_eq!(desired.len(), 2);
    assert_eq!(desired[0].slot_id, SlotId("slot-0001".into()));
    assert_eq!(desired[0].machine_id, MachineId("machine-a".into()));
    assert_eq!(desired[1].slot_id, SlotId("slot-0002".into()));
    assert_eq!(desired[1].machine_id, MachineId("machine-a".into()));
}

#[tokio::test]
async fn resolve_plan_marks_matching_release_unchanged() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.27",
    )]);
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one manifest service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Active))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-b",
                "inst-1",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    assert_eq!(
        service_plan.action,
        crate::model::DeployChangeKind::Unchanged
    );
    assert_eq!(service_plan.service, "api");
}

#[tokio::test]
async fn resolve_plan_reuses_slot_machine_when_revision_changes() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.28",
    )]);
    let old_spec = test_service_spec("api", Placement::Replicated { count: 1 }, "nginx:1.27");
    let old_revision_hash = old_spec.revision_hash().expect("old revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Active))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &old_revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-b",
                "inst-1",
                &old_revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(service_plan.action, crate::model::DeployChangeKind::Replace);
    assert_eq!(slot_plan.machine_id, MachineId("machine-b".into()));
}

#[tokio::test]
async fn resolve_plan_moves_replacement_off_region_draining_machine() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.28",
    )]);
    let old_spec = test_service_spec("api", Placement::Replicated { count: 1 }, "nginx:1.27");
    let old_revision_hash = old_spec.revision_hash().expect("old revision hash");

    store
        .upsert_self_machine(&test_machine_in_region(
            "compute",
            MachineLifecycle::Active,
            RegionRole::Compute,
        ))
        .await
        .expect("seed compute");
    store
        .upsert_self_machine(&test_machine_in_region(
            "region-draining",
            MachineLifecycle::Active,
            RegionRole::Draining,
        ))
        .await
        .expect("seed region-draining");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &old_revision_hash,
            vec![test_slot(
                "slot-0001",
                "region-draining",
                "inst-1",
                &old_revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(slot_plan.action, DeployChangeKind::Replace);
    assert_eq!(slot_plan.machine_id, MachineId("compute".into()));
}

#[tokio::test]
async fn resolve_plan_pins_new_volume_to_existing_slot_machine() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let mut service = test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17");
    service.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![service]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    let old_spec = test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:16");
    let old_revision_hash = old_spec.revision_hash().expect("old revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Active))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "db",
            &old_revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-b",
                "inst-1",
                &old_revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [volume] = plan.volumes() else {
        panic!("expected one volume");
    };
    assert_eq!(volume.machine_id, MachineId("machine-b".into()));
    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(slot_plan.machine_id, MachineId("machine-b".into()));
}

#[tokio::test]
async fn resolve_plan_keeps_existing_volume_on_region_draining_machine() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let mut service = test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17");
    service.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![service]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one manifest service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");

    store
        .upsert_self_machine(&test_machine_in_region(
            "region-draining",
            MachineLifecycle::Active,
            RegionRole::Draining,
        ))
        .await
        .expect("seed machine");
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "region-draining",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "db",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "region-draining",
                "inst-1",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [volume] = plan.volumes() else {
        panic!("expected one volume");
    };
    assert_eq!(volume.machine_id, MachineId("region-draining".into()));
    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(slot_plan.machine_id, MachineId("region-draining".into()));
}

#[tokio::test]
async fn resolve_plan_moves_volume_backed_service_from_draining_machine() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let mut service = test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17");
    service.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![service]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one manifest service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Draining))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "db",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-1",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [volume] = plan.volumes() else {
        panic!("expected one planned volume");
    };
    assert_eq!(volume.machine_id, MachineId("machine-b".into()));
    assert_eq!(
        volume
            .movement
            .as_ref()
            .map(|movement| (&movement.from_machine, &movement.to_machine)),
        Some((
            &MachineId("machine-a".into()),
            &MachineId("machine-b".into())
        ))
    );

    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(service_plan.action, DeployChangeKind::Replace);
    assert_eq!(slot_plan.action, DeployChangeKind::Replace);
    assert_eq!(slot_plan.machine_id, MachineId("machine-b".into()));
}

#[tokio::test]
async fn resolve_plan_moves_draining_volume_only_to_storage_capable_target() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let mut service = test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17");
    service.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![service]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one manifest service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Draining))
        .await
        .expect("seed machine-a");
    let mut compute_only = test_machine("machine-b", MachineLifecycle::Active);
    compute_only.storage = false;
    store
        .upsert_self_machine(&compute_only)
        .await
        .expect("seed machine-b");
    store
        .upsert_self_machine(&test_machine("machine-c", MachineLifecycle::Active))
        .await
        .expect("seed machine-c");
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "db",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-1",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [volume] = plan.volumes() else {
        panic!("expected one planned volume");
    };
    assert_eq!(volume.machine_id, MachineId("machine-c".into()));
    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(slot_plan.machine_id, MachineId("machine-c".into()));
}

#[tokio::test]
async fn resolve_plan_moves_unattached_declared_volume_from_draining_machine() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let mut manifest = test_manifest(Vec::new());
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Draining))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    seed_volume(&store, &manifest.namespace, "data", "machine-a").await;

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [volume] = plan.volumes() else {
        panic!("expected one planned volume");
    };
    assert_eq!(volume.machine_id, MachineId("machine-b".into()));
    assert!(volume.attached_services.is_empty());
    assert_eq!(
        volume
            .movement
            .as_ref()
            .map(|movement| (&movement.from_machine, &movement.to_machine)),
        Some((
            &MachineId("machine-a".into()),
            &MachineId("machine-b".into())
        ))
    );
    let preview = plan.to_preview(Vec::new());
    let [volume_move] = preview.volume_moves.as_slice() else {
        panic!("expected one preview volume move");
    };
    assert_eq!(volume_move.volume, "data");
    assert!(volume_move.attached_services.is_empty());
}

#[tokio::test]
async fn resolve_plan_moves_draining_volume_to_existing_service_volume_pin() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let mut service = test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17");
    service.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    service.template.mounts.push(Mount {
        source: MountSource::Volume("wal".into()),
        target: "/var/lib/postgresql/wal".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![service]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest
        .volumes
        .push(test_volume("wal", VolumeScope::Single));
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one manifest service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Draining))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    store
        .upsert_self_machine(&test_machine("machine-c", MachineLifecycle::Active))
        .await
        .expect("seed machine-c");
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "wal",
        "machine-c",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "db",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-1",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let data = plan
        .volumes()
        .iter()
        .find(|volume| volume.declaration.name == "data")
        .expect("data volume");
    assert_eq!(data.machine_id, MachineId("machine-c".into()));
    assert_eq!(
        data.movement
            .as_ref()
            .map(|movement| (&movement.from_machine, &movement.to_machine)),
        Some((
            &MachineId("machine-a".into()),
            &MachineId("machine-c".into())
        ))
    );
    let wal = plan
        .volumes()
        .iter()
        .find(|volume| volume.declaration.name == "wal")
        .expect("wal volume");
    assert_eq!(wal.machine_id, MachineId("machine-c".into()));
    assert!(wal.movement.is_none());
    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(slot_plan.machine_id, MachineId("machine-c".into()));
}

#[tokio::test]
async fn resolve_plan_moves_draining_volume_to_pending_sibling_move_target() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let mut service = test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17");
    service.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    service.template.mounts.push(Mount {
        source: MountSource::Volume("wal".into()),
        target: "/var/lib/postgresql/wal".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![service]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest
        .volumes
        .push(test_volume("wal", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "wal".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-d".into(),
                to_machine: "machine-c".into(),
            },
        }],
        phases: Vec::new(),
    });
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one manifest service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Draining))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    store
        .upsert_self_machine(&test_machine("machine-c", MachineLifecycle::Active))
        .await
        .expect("seed machine-c");
    store
        .upsert_self_machine(&test_machine("machine-d", MachineLifecycle::Active))
        .await
        .expect("seed machine-d");
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "wal",
        "machine-d",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "db",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-1",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let data = plan
        .volumes()
        .iter()
        .find(|volume| volume.declaration.name == "data")
        .expect("data volume");
    assert_eq!(data.machine_id, MachineId("machine-c".into()));
    assert_eq!(
        data.movement
            .as_ref()
            .map(|movement| (&movement.from_machine, &movement.to_machine)),
        Some((
            &MachineId("machine-a".into()),
            &MachineId("machine-c".into())
        ))
    );
    let wal = plan
        .volumes()
        .iter()
        .find(|volume| volume.declaration.name == "wal")
        .expect("wal volume");
    assert_eq!(wal.machine_id, MachineId("machine-c".into()));
    assert_eq!(
        wal.movement
            .as_ref()
            .map(|movement| (&movement.from_machine, &movement.to_machine)),
        Some((
            &MachineId("machine-d".into()),
            &MachineId("machine-c".into())
        ))
    );
    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(slot_plan.machine_id, MachineId("machine-c".into()));
}

#[tokio::test]
async fn resolve_plan_preserves_invalid_pending_sibling_move_error() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let mut service = test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17");
    service.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    service.template.mounts.push(Mount {
        source: MountSource::Volume("wal".into()),
        target: "/var/lib/postgresql/wal".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![service]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest
        .volumes
        .push(test_volume("wal", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "wal".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-x".into(),
                to_machine: "machine-c".into(),
            },
        }],
        phases: Vec::new(),
    });

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Draining))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    store
        .upsert_self_machine(&test_machine("machine-c", MachineLifecycle::Active))
        .await
        .expect("seed machine-c");
    store
        .upsert_self_machine(&test_machine("machine-d", MachineLifecycle::Active))
        .await
        .expect("seed machine-d");
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "wal",
        "machine-d",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("invalid explicit sibling move should still fail");

    assert!(matches!(
        error,
        Error::Deploy(DeployError::VolumeMoveSourceMismatch {
            volume,
            expected_machine,
            actual_machine,
        }) if volume == "wal" && expected_machine == "machine-x" && actual_machine == "machine-d"
    ));
}

#[tokio::test]
async fn resolve_plan_moves_existing_volume_and_attached_service_to_target_machine() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-a".into(),
                to_machine: "machine-b".into(),
            },
        }],
        phases: Vec::new(),
    });
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "db",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-1",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve move plan");

    let [volume] = plan.volumes() else {
        panic!("expected one planned volume");
    };
    assert_eq!(volume.machine_id, MachineId("machine-b".into()));
    assert_eq!(
        volume
            .movement
            .as_ref()
            .map(|movement| (&movement.from_machine, &movement.to_machine)),
        Some((
            &MachineId("machine-a".into()),
            &MachineId("machine-b".into())
        ))
    );
    assert!(plan.participants().contains(&MachineId("machine-a".into())));
    assert!(plan.participants().contains(&MachineId("machine-b".into())));

    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(service_plan.action, DeployChangeKind::Replace);
    assert_eq!(slot_plan.action, DeployChangeKind::Replace);
    assert_eq!(slot_plan.machine_id, MachineId("machine-b".into()));

    let preview = plan.to_preview(Vec::new());
    let [volume_move] = preview.volume_moves.as_slice() else {
        panic!("expected one preview volume move");
    };
    assert_eq!(volume_move.volume, "data");
    assert_eq!(volume_move.from_machine, MachineId("machine-a".into()));
    assert_eq!(volume_move.to_machine, MachineId("machine-b".into()));
    assert_eq!(volume_move.attached_services, vec!["db"]);
    let [phase] = preview.phases.as_slice() else {
        panic!("expected one default deploy phase");
    };
    assert_eq!(phase.phase_id, DeployPhaseId("deploy".into()));
    assert_eq!(phase.name, "Deploy");
    assert_eq!(phase.order, 0);
    assert_eq!(phase.commit_policy, DeployPhaseCommitPolicy::EndOfDeploy);
    assert_eq!(phase.rollback_policy, DeployPhaseRollbackPolicy::Reversible);
    assert_eq!(phase.advance_policy, DeployPhaseAdvancePolicy::Immediate);
    assert_eq!(
        phase.participants,
        vec![MachineId("machine-a".into()), MachineId("machine-b".into())]
    );
    assert!(matches!(
        phase.work.as_slice(),
        [
            DeployPhaseWork::VolumeMove {
                volume,
                from_machine,
                to_machine,
                attached_services
            },
            DeployPhaseWork::Service { service, action }
        ] if volume == "data"
            && from_machine == &MachineId("machine-a".into())
            && to_machine == &MachineId("machine-b".into())
            && attached_services.as_slice() == ["db"]
            && service == "db"
            && *action == DeployChangeKind::Replace
    ));
}

#[tokio::test]
async fn resolve_plan_treats_volume_move_to_same_machine_as_noop() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-a".into(),
                to_machine: "machine-a".into(),
            },
        }],
        phases: Vec::new(),
    });
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "db",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-1",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve no-op move plan");

    let [volume] = plan.volumes() else {
        panic!("expected one planned volume");
    };
    assert_eq!(volume.machine_id, MachineId("machine-a".into()));
    assert_eq!(volume.movement, None);
    assert!(plan.to_preview(Vec::new()).volume_moves.is_empty());
    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    assert_eq!(service_plan.action, DeployChangeKind::Unchanged);
}

#[tokio::test]
async fn resolve_plan_rejects_volume_move_source_mismatch() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-b".into(),
                to_machine: "machine-a".into(),
            },
        }],
        phases: Vec::new(),
    });
    seed_volume(&store, &manifest.namespace, "data", "machine-a").await;

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("source mismatch should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::VolumeMoveSourceMismatch {
            volume: "data".into(),
            expected_machine: "machine-b".into(),
            actual_machine: "machine-a".into()
        })
    );
}

#[tokio::test]
async fn resolve_plan_rejects_volume_move_to_missing_ineligible_or_compute_only_target() {
    let local_machine_id = MachineId("local".into());
    for (target, maybe_machine, expected) in [
        (
            "missing",
            None,
            Error::Deploy(DeployError::VolumeMoveTargetMissing {
                volume: "data".into(),
                machine_id: "missing".into(),
            }),
        ),
        (
            "standby",
            Some({
                let mut machine = test_machine("standby", MachineLifecycle::Standby);
                machine.storage = true;
                machine
            }),
            Error::Deploy(DeployError::VolumeMoveTargetIneligible {
                volume: "data".into(),
                machine_id: "standby".into(),
            }),
        ),
        (
            "compute-only",
            Some({
                let mut machine = test_machine("compute-only", MachineLifecycle::Active);
                machine.storage = false;
                machine
            }),
            Error::Deploy(DeployError::VolumeMoveTargetNotStorageCapable {
                volume: "data".into(),
                machine_id: "compute-only".into(),
            }),
        ),
    ] {
        let store = seeded_store_with_machines(&["machine-a"]).await;
        if let Some(machine) = maybe_machine {
            store
                .upsert_self_machine(&machine)
                .await
                .expect("seed target");
        }
        let mut manifest = volume_manifest();
        manifest.intent = Some(DeployIntent {
            services: Vec::new(),
            volumes: vec![VolumeIntentHint {
                volume: "data".into(),
                intent: VolumeIntent::Move {
                    from_machine: "machine-a".into(),
                    to_machine: target.into(),
                },
            }],
            phases: Vec::new(),
        });
        seed_volume(&store, &manifest.namespace, "data", "machine-a").await;

        let error = resolve_plan(&store, &local_machine_id, &manifest)
            .await
            .expect_err("bad target should fail");

        assert_eq!(error, expected);
    }
}

#[tokio::test]
async fn resolve_plan_rejects_volume_move_for_shared_volume() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = test_manifest(Vec::new());
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Shared));
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-a".into(),
                to_machine: "machine-b".into(),
            },
        }],
        phases: Vec::new(),
    });
    seed_volume_with_scope(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        VolumeScope::Shared,
    )
    .await;

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("shared volume move should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::VolumeMoveRequiresSingleScope {
            volume: "data".into()
        })
    );
}

#[tokio::test]
async fn resolve_plan_rejects_volume_move_for_global_attached_service() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let mut service = test_service_spec("db", Placement::Global, "postgres:17");
    service.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![service]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-a".into(),
                to_machine: "machine-b".into(),
            },
        }],
        phases: Vec::new(),
    });
    seed_volume(&store, &manifest.namespace, "data", "machine-a").await;

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("global service cannot attach a moved single-scope volume");

    match error {
        Error::Deploy(DeployError::ManifestInvalid { message }) => {
            assert!(
                message.contains("cannot use global placement with managed volumes"),
                "got: {message}"
            );
        }
        other => panic!("expected manifest validation failure, got: {other:?}"),
    }
}

#[tokio::test]
async fn resolve_plan_rejects_service_with_volumes_on_different_machines() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let mut service = test_service_spec("api", Placement::Replicated { count: 1 }, "nginx:1.28");
    service.template.mounts.push(Mount {
        source: MountSource::Volume("left".into()),
        target: "/left".into(),
        readonly: false,
    });
    service.template.mounts.push(Mount {
        source: MountSource::Volume("right".into()),
        target: "/right".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![service]);
    manifest
        .volumes
        .push(test_volume("left", VolumeScope::Single));
    manifest
        .volumes
        .push(test_volume("right", VolumeScope::Single));

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Active))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    seed_volume(&store, &manifest.namespace, "left", "machine-a").await;
    seed_volume(&store, &manifest.namespace, "right", "machine-b").await;

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("volume machine conflict should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::ServiceVolumesOnDifferentMachines {
            service: "api".into()
        })
    );
}

#[tokio::test]
async fn apply_commits_volume_records_and_sends_volume_payload_to_startup() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = volume_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller.clone());

    let first =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("first deploy");

    let requests = controller.start_requests().await;
    let [request] = requests.as_slice() else {
        panic!("expected one start request");
    };
    let volumes: Vec<VolumeDeclaration> =
        serde_json::from_str(&request.volumes_json).expect("volumes json");
    let [volume] = volumes.as_slice() else {
        panic!("expected one volume declaration");
    };
    assert_eq!(volume.name, "data");
    assert_eq!(volume.scope, VolumeScope::Single);
    assert_eq!(request.service, "db");

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected one committed volume");
    };
    assert_eq!(record.volume_name, "data");
    assert_eq!(record.machine_id, MachineId("machine-a".into()));
    assert_eq!(record.quota, "1G");
    assert_eq!(record.mode, "0750");
    assert_eq!(record.owner, "999:999");
    assert_eq!(record.attached_services, vec!["db"]);
    assert_eq!(record.created_by_deploy_id, first.deploy_id);
    let first_created_at = record.created_at;
    let first_created_by = record.created_by_deploy_id.clone();

    let second_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("second plan");
    let second =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, second_plan)
            .await
            .expect("second deploy");
    assert_eq!(controller.start_count(), 1);

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected one committed volume after redeploy");
    };
    assert_eq!(record.created_at, first_created_at);
    assert_eq!(record.created_by_deploy_id, first_created_by);
    assert_eq!(record.last_modified_by_deploy_id, first.deploy_id);
    assert_ne!(record.last_modified_by_deploy_id, second.deploy_id);
}

#[tokio::test]
async fn preview_plans_volume_clone_on_source_machine() {
    let (store, _backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let source_namespace = Namespace("prod".into());
    seed_volume(&store, &source_namespace, "data", "machine-b").await;
    let mut manifest = volume_manifest();
    manifest.namespace = Namespace("pr-39".into());
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Clone {
                source_namespace: source_namespace.clone(),
                source_volume: "data".into(),
                data_policy: VolumeCloneDataPolicy::Raw,
                consistency: VolumeCloneConsistency::CrashConsistent,
            },
        }],
        phases: Vec::new(),
    });

    let preview = preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect("clone preview");

    let [clone] = preview.volume_clones.as_slice() else {
        panic!("expected one volume clone");
    };
    assert_eq!(clone.volume, "data");
    assert_eq!(clone.source_namespace, source_namespace);
    assert_eq!(clone.source_volume, "data");
    assert_eq!(clone.source_machine, MachineId("machine-b".into()));
    assert_eq!(clone.target_machine, MachineId("machine-b".into()));
    assert_eq!(clone.data_policy, VolumeCloneDataPolicy::Raw);
    assert_eq!(clone.consistency, VolumeCloneConsistency::CrashConsistent);
    let [phase] = preview.phases.as_slice() else {
        panic!("expected synthetic deploy phase");
    };
    assert!(matches!(
        phase.work.as_slice(),
        [
            DeployPhaseWork::VolumeClone { volume, .. },
            DeployPhaseWork::Service { service, .. },
        ] if volume == "data" && service == "db"
    ));
}

#[tokio::test]
async fn apply_executes_volume_clone_before_startup_and_commits_lineage() {
    let (store, _backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let source_namespace = Namespace("prod".into());
    seed_volume(&store, &source_namespace, "data", "machine-b").await;
    let mut manifest = volume_manifest();
    manifest.namespace = Namespace("pr-39".into());
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Clone {
                source_namespace: source_namespace.clone(),
                source_volume: "data".into(),
                data_policy: VolumeCloneDataPolicy::Raw,
                consistency: VolumeCloneConsistency::CrashConsistent,
            },
        }],
        phases: Vec::new(),
    });
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller.clone());

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("clone deploy");

    assert_eq!(result.state, DeployState::Committed);
    assert_eq!(controller.clone_count(), 1);
    assert_eq!(controller.start_count(), 1);
    let clone_requests = controller.clone_requests().await;
    let [clone_request] = clone_requests.as_slice() else {
        panic!("expected clone request");
    };
    assert_eq!(clone_request.volume, "data");
    assert_eq!(clone_request.source_namespace, source_namespace);
    assert_eq!(clone_request.source_volume, "data");
    assert_eq!(clone_request.quota, "1G");
    let log = controller.operation_log().await;
    let clone_index = log
        .iter()
        .position(|entry| entry.starts_with("clone:data:machine-b:prod/data"))
        .expect("clone operation logged");
    let start_index = log
        .iter()
        .position(|entry| entry.starts_with("start:db:"))
        .expect("start operation logged");
    assert!(clone_index < start_index);

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list target volumes");
    let [record] = records.as_slice() else {
        panic!("expected cloned volume record");
    };
    assert_eq!(record.machine_id, MachineId("machine-b".into()));
    assert_eq!(record.created_by_deploy_id, result.deploy_id);

    let lineage = store
        .list_volume_branches(&manifest.namespace)
        .await
        .expect("list volume branches");
    let [branch] = lineage.as_slice() else {
        panic!("expected volume branch lineage");
    };
    assert_eq!(branch.volume_name, "data");
    assert_eq!(branch.source_namespace, source_namespace);
    assert_eq!(branch.source_volume_name, "data");
    assert_eq!(branch.source_machine, MachineId("machine-b".into()));
    assert_eq!(branch.target_machine, MachineId("machine-b".into()));
    assert_eq!(branch.data_policy, VolumeCloneDataPolicy::Raw);
    assert_eq!(branch.consistency, VolumeCloneConsistency::CrashConsistent);
    assert_eq!(branch.snapshot_guid, 84);
    assert_eq!(branch.deploy_id, result.deploy_id);
    assert_eq!(branch.commit_deploy_id, result.deploy_id);
    assert_eq!(branch.phase_id, Some(DeployPhaseId("deploy".into())));

    let reapply_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("lineage-matched clone reapply plan");
    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, reapply_plan)
        .await
        .expect("lineage-matched clone reapply");
    assert_eq!(controller.clone_count(), 1);

    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Draining))
        .await
        .expect("mark clone source machine draining");
    let move_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone lineage survives inferred move plan");
    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, move_plan)
        .await
        .expect("move cloned volume");
    assert_eq!(controller.clone_count(), 1);
    assert_eq!(controller.move_count(), 1);
    let moved_records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list moved clone volume");
    let [moved_record] = moved_records.as_slice() else {
        panic!("expected moved clone volume record");
    };
    assert_eq!(moved_record.machine_id, MachineId("machine-a".into()));

    let moved_reapply_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone lineage reapply after move");
    apply_with_initial_plan(
        &store,
        &factory,
        &local_machine_id,
        &manifest,
        moved_reapply_plan,
    )
    .await
    .expect("clone reapply after move");
    assert_eq!(controller.clone_count(), 1);
}

#[tokio::test]
async fn preview_rejects_volume_clone_when_target_exists() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let source_namespace = Namespace("prod".into());
    seed_volume(&store, &source_namespace, "data", "machine-a").await;
    let mut manifest = volume_manifest();
    seed_volume(&store, &manifest.namespace, "data", "machine-a").await;
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Clone {
                source_namespace,
                source_volume: "data".into(),
                data_policy: VolumeCloneDataPolicy::Raw,
                consistency: VolumeCloneConsistency::CrashConsistent,
            },
        }],
        phases: Vec::new(),
    });

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("target clone collision should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::VolumeCloneTargetExists {
            volume: "data".into()
        })
    );
}

#[tokio::test]
async fn apply_cleans_uncommitted_volume_clone_when_startup_fails() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let source_namespace = Namespace("prod".into());
    seed_volume(&store, &source_namespace, "data", "machine-a").await;
    let web = test_service_spec("web", Placement::Replicated { count: 1 }, "nginx:1.27");
    let mut manifest = test_manifest(vec![web]);
    manifest.namespace = Namespace("pr-39".into());
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Clone {
                source_namespace,
                source_volume: "data".into(),
                data_policy: VolumeCloneDataPolicy::Raw,
                consistency: VolumeCloneConsistency::CrashConsistent,
            },
        }],
        phases: Vec::new(),
    });
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let controller = FakeController {
        fail_start_service: Some("web".into()),
        ..FakeController::default()
    };
    let factory = FakeParticipantClient::new(controller.clone());

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect_err("startup failure should fail deploy");

    assert_eq!(controller.clone_count(), 1);
    assert_eq!(controller.clone_cleanup_count(), 1);
    assert!(
        store
            .get_volume(&manifest.namespace, "data")
            .await
            .expect("get volume")
            .is_none()
    );
}

#[tokio::test]
async fn apply_keeps_volume_clone_when_attached_service_start_returns_error() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let source_namespace = Namespace("prod".into());
    seed_volume(&store, &source_namespace, "data", "machine-a").await;
    let mut manifest = volume_manifest();
    manifest.namespace = Namespace("pr-39".into());
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Clone {
                source_namespace,
                source_volume: "data".into(),
                data_policy: VolumeCloneDataPolicy::Raw,
                consistency: VolumeCloneConsistency::CrashConsistent,
            },
        }],
        phases: Vec::new(),
    });
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let controller = FakeController {
        fail_start_after_create_service: Some("db".into()),
        ..FakeController::default()
    };
    let factory = FakeParticipantClient::new(controller.clone());

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect_err("attached service startup failure should fail deploy");

    assert_eq!(controller.clone_count(), 1);
    assert_eq!(controller.clone_cleanup_count(), 0);
    let log = controller.operation_log().await;
    assert!(
        log.iter().any(|entry| entry.starts_with("start:db:")),
        "expected db start attempt before deploy failed: {log:?}"
    );
}

#[tokio::test]
async fn apply_keeps_started_uncheckpointed_volume_clone_when_later_phase_fails() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let source_namespace = Namespace("prod".into());
    seed_volume(&store, &source_namespace, "data", "machine-a").await;

    let mut db = test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17");
    db.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let web = test_service_spec("web", Placement::Replicated { count: 1 }, "nginx:1.27");
    let mut manifest = test_manifest(vec![db, web]);
    manifest.namespace = Namespace("pr-39".into());
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Clone {
                source_namespace,
                source_volume: "data".into(),
                data_policy: VolumeCloneDataPolicy::Raw,
                consistency: VolumeCloneConsistency::CrashConsistent,
            },
        }],
        phases: vec![
            DeployPhaseIntent {
                phase_id: "db".into(),
                name: Some("Database".into()),
                after: Vec::new(),
                services: vec!["db".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
            DeployPhaseIntent {
                phase_id: "web".into(),
                name: Some("Web".into()),
                after: vec!["db".into()],
                services: vec!["web".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
        ],
    });
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let controller = FakeController {
        fail_start_service: Some("web".into()),
        ..FakeController::default()
    };
    let factory = FakeParticipantClient::new(controller.clone());

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect_err("later phase startup failure should fail deploy");

    assert_eq!(controller.clone_count(), 1);
    assert_eq!(controller.clone_cleanup_count(), 0);
    let log = controller.operation_log().await;
    assert!(
        log.iter().any(|entry| entry.starts_with("start:db:")),
        "expected db to start before later phase failed: {log:?}"
    );
}

#[tokio::test]
async fn apply_keeps_started_volume_clone_when_same_phase_later_service_fails() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let source_namespace = Namespace("prod".into());
    seed_volume(&store, &source_namespace, "data", "machine-a").await;

    let mut db = test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17");
    db.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let web = test_service_spec("web", Placement::Replicated { count: 1 }, "nginx:1.27");
    let mut manifest = test_manifest(vec![db, web]);
    manifest.namespace = Namespace("pr-39".into());
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Clone {
                source_namespace,
                source_volume: "data".into(),
                data_policy: VolumeCloneDataPolicy::Raw,
                consistency: VolumeCloneConsistency::CrashConsistent,
            },
        }],
        phases: vec![DeployPhaseIntent {
            phase_id: "app".into(),
            name: Some("App".into()),
            after: Vec::new(),
            services: vec!["db".into(), "web".into()],
            volumes: Vec::new(),
            commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
            rollback_policy: DeployPhaseRollbackPolicy::Reversible,
            advance_policy: DeployPhaseAdvancePolicy::Immediate,
        }],
    });
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let controller = FakeController {
        fail_start_service: Some("web".into()),
        ..FakeController::default()
    };
    let factory = FakeParticipantClient::new(controller.clone());

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect_err("same phase startup failure should fail deploy");

    assert_eq!(controller.clone_count(), 1);
    assert_eq!(controller.clone_cleanup_count(), 0);
    let log = controller.operation_log().await;
    let db_start = log
        .iter()
        .position(|entry| entry.starts_with("start:db:"))
        .expect("expected db to start before web failed");
    let web_start = log
        .iter()
        .position(|entry| entry.starts_with("start:web:"))
        .expect("expected web start attempt to fail deploy");
    assert!(
        db_start < web_start,
        "expected db start to precede failing web start: {log:?}"
    );
}

#[tokio::test]
async fn apply_drains_live_uncommitted_volume_clone_writers_before_retrying_clone() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let source_namespace = Namespace("prod".into());
    seed_volume(&store, &source_namespace, "data", "machine-a").await;

    let mut db = test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17");
    db.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let web = test_service_spec("web", Placement::Replicated { count: 1 }, "nginx:1.27");
    let mut manifest = test_manifest(vec![db, web]);
    manifest.namespace = Namespace("pr-39".into());
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Clone {
                source_namespace,
                source_volume: "data".into(),
                data_policy: VolumeCloneDataPolicy::Raw,
                consistency: VolumeCloneConsistency::CrashConsistent,
            },
        }],
        phases: vec![DeployPhaseIntent {
            phase_id: "app".into(),
            name: Some("App".into()),
            after: Vec::new(),
            services: vec!["db".into(), "web".into()],
            volumes: Vec::new(),
            commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
            rollback_policy: DeployPhaseRollbackPolicy::Reversible,
            advance_policy: DeployPhaseAdvancePolicy::Immediate,
        }],
    });

    let first_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let first_controller = FakeController {
        fail_start_service: Some("web".into()),
        ..FakeController::default()
    };
    let first_factory = FakeParticipantClient::new(first_controller.clone());
    apply_with_initial_plan(
        &store,
        &first_factory,
        &local_machine_id,
        &manifest,
        first_plan,
    )
    .await
    .expect_err("first deploy leaves started uncommitted clone writer");
    assert_eq!(first_controller.clone_cleanup_count(), 0);

    let retry_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("retry clone plan");
    let retry_controller = FakeController::default();
    retry_controller
        .set_inspect_instances(vec![test_instance_status(
            &manifest.namespace,
            "db",
            "slot-0001",
            "machine-a",
            "inst-db-old",
            "fake-revision",
        )])
        .await;
    let retry_factory = FakeParticipantClient::new(retry_controller.clone());

    apply_with_initial_plan(
        &store,
        &retry_factory,
        &local_machine_id,
        &manifest,
        retry_plan,
    )
    .await
    .expect("retry should drain old writer before cloning");

    assert_eq!(retry_controller.clone_count(), 1);
    assert!(retry_controller.drain_count() >= 1);
    assert!(retry_controller.remove_count() >= 1);
    let log = retry_controller.operation_log().await;
    let drain = log
        .iter()
        .position(|entry| entry == "drain:inst-db-old")
        .expect("old writer should be drained before clone");
    let remove = log
        .iter()
        .position(|entry| entry == "remove:inst-db-old")
        .expect("old writer should be removed before clone");
    let clone = log
        .iter()
        .position(|entry| entry.starts_with("clone:data:machine-a:prod/data"))
        .expect("clone should run after old writer is removed");
    assert!(
        drain < remove && remove < clone,
        "expected old writer cleanup before clone retry: {log:?}"
    );
}

#[tokio::test]
async fn apply_drains_removed_uncommitted_volume_clone_candidates_before_retrying_clone() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let source_namespace = Namespace("prod".into());
    seed_volume(&store, &source_namespace, "data", "machine-a").await;

    let mut db = test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17");
    db.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let web = test_service_spec("web", Placement::Replicated { count: 1 }, "nginx:1.27");
    let mut manifest = test_manifest(vec![db, web]);
    manifest.namespace = Namespace("pr-39".into());
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Clone {
                source_namespace,
                source_volume: "data".into(),
                data_policy: VolumeCloneDataPolicy::Raw,
                consistency: VolumeCloneConsistency::CrashConsistent,
            },
        }],
        phases: vec![DeployPhaseIntent {
            phase_id: "app".into(),
            name: Some("App".into()),
            after: Vec::new(),
            services: vec!["db".into(), "web".into()],
            volumes: Vec::new(),
            commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
            rollback_policy: DeployPhaseRollbackPolicy::Reversible,
            advance_policy: DeployPhaseAdvancePolicy::Immediate,
        }],
    });

    let first_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let first_controller = FakeController {
        fail_start_service: Some("web".into()),
        ..FakeController::default()
    };
    let first_factory = FakeParticipantClient::new(first_controller.clone());
    apply_with_initial_plan(
        &store,
        &first_factory,
        &local_machine_id,
        &manifest,
        first_plan,
    )
    .await
    .expect_err("first deploy leaves started uncommitted clone candidate");
    assert_eq!(first_controller.clone_cleanup_count(), 0);

    let mut retry_manifest = manifest.clone();
    retry_manifest
        .services
        .retain(|service| service.name.as_str() == "web");
    let Some(intent) = retry_manifest.intent.as_mut() else {
        panic!("expected clone intent");
    };
    intent.phases.clear();

    let retry_plan = resolve_plan(&store, &local_machine_id, &retry_manifest)
        .await
        .expect("retry clone plan");
    let retry_controller = FakeController::default();
    retry_controller
        .set_inspect_instances(vec![test_instance_status(
            &retry_manifest.namespace,
            "db",
            "slot-0001",
            "machine-a",
            "inst-db-old",
            "fake-revision",
        )])
        .await;
    let retry_factory = FakeParticipantClient::new(retry_controller.clone());

    apply_with_initial_plan(
        &store,
        &retry_factory,
        &local_machine_id,
        &retry_manifest,
        retry_plan,
    )
    .await
    .expect("retry should drain removed stale candidate before cloning");

    assert_eq!(retry_controller.clone_count(), 1);
    let log = retry_controller.operation_log().await;
    let drain = log
        .iter()
        .position(|entry| entry == "drain:inst-db-old")
        .expect("removed stale candidate should be drained before clone");
    let remove = log
        .iter()
        .position(|entry| entry == "remove:inst-db-old")
        .expect("removed stale candidate should be removed before clone");
    let clone = log
        .iter()
        .position(|entry| entry.starts_with("clone:data:machine-a:prod/data"))
        .expect("clone should run after stale candidate is removed");
    assert!(
        drain < remove && remove < clone,
        "expected removed stale candidate cleanup before clone retry: {log:?}"
    );
}

#[tokio::test]
async fn apply_does_not_drain_committed_service_before_creating_new_clone() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let source_namespace = Namespace("prod".into());
    seed_volume(&store, &source_namespace, "data", "machine-a").await;

    let mut db = test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17");
    db.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![db]);
    manifest.namespace = Namespace("pr-39".into());
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Clone {
                source_namespace,
                source_volume: "data".into(),
                data_policy: VolumeCloneDataPolicy::Raw,
                consistency: VolumeCloneConsistency::CrashConsistent,
            },
        }],
        phases: Vec::new(),
    });
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "db",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-db-committed",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let controller = FakeController {
        fail_clone_volume: Some("data".into()),
        ..FakeController::default()
    };
    controller
        .set_inspect_instances(vec![test_instance_status(
            &manifest.namespace,
            "db",
            "slot-0001",
            "machine-a",
            "inst-db-committed",
            &revision_hash,
        )])
        .await;
    let factory = FakeParticipantClient::new(controller.clone());

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect_err("clone failure should fail deploy");

    assert_eq!(controller.clone_count(), 1);
    assert_eq!(controller.drain_count(), 0);
    assert_eq!(controller.remove_count(), 0);
}

#[tokio::test]
async fn apply_surfaces_uncommitted_volume_clone_cleanup_failures() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let source_namespace = Namespace("prod".into());
    seed_volume(&store, &source_namespace, "data", "machine-a").await;
    let web = test_service_spec("web", Placement::Replicated { count: 1 }, "nginx:1.27");
    let mut manifest = test_manifest(vec![web]);
    manifest.namespace = Namespace("pr-39".into());
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Clone {
                source_namespace,
                source_volume: "data".into(),
                data_policy: VolumeCloneDataPolicy::Raw,
                consistency: VolumeCloneConsistency::CrashConsistent,
            },
        }],
        phases: Vec::new(),
    });
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let controller = FakeController {
        fail_start_service: Some("web".into()),
        fail_cleanup_clone_volume: Some("data".into()),
        ..FakeController::default()
    };
    let factory = FakeParticipantClient::new(controller.clone());

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("startup failure should fail deploy");

    assert_eq!(controller.clone_count(), 1);
    assert_eq!(controller.clone_cleanup_count(), 1);
    assert!(
        error
            .to_string()
            .contains("uncommitted volume clone cleanup failed"),
        "got: {error}"
    );
}

#[tokio::test]
async fn apply_cleans_successful_volume_clones_when_later_clone_fails() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let source_namespace = Namespace("prod".into());
    seed_volume(&store, &source_namespace, "data", "machine-a").await;
    seed_volume(&store, &source_namespace, "cache", "machine-a").await;
    let mut manifest = volume_manifest();
    manifest.namespace = Namespace("pr-39".into());
    manifest
        .volumes
        .push(test_volume("cache", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![
            VolumeIntentHint {
                volume: "data".into(),
                intent: VolumeIntent::Clone {
                    source_namespace: source_namespace.clone(),
                    source_volume: "data".into(),
                    data_policy: VolumeCloneDataPolicy::Raw,
                    consistency: VolumeCloneConsistency::CrashConsistent,
                },
            },
            VolumeIntentHint {
                volume: "cache".into(),
                intent: VolumeIntent::Clone {
                    source_namespace,
                    source_volume: "cache".into(),
                    data_policy: VolumeCloneDataPolicy::Raw,
                    consistency: VolumeCloneConsistency::CrashConsistent,
                },
            },
        ],
        phases: Vec::new(),
    });
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let controller = FakeController {
        fail_clone_volume: Some("cache".into()),
        ..FakeController::default()
    };
    let factory = FakeParticipantClient::new(controller.clone());

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect_err("second clone failure should fail deploy");

    assert_eq!(controller.clone_count(), 2);
    assert_eq!(controller.clone_cleanup_count(), 2);
    assert_eq!(controller.start_count(), 0);
    assert!(
        store
            .list_volume_branches(&manifest.namespace)
            .await
            .expect("list branches")
            .is_empty()
    );
}

#[tokio::test]
async fn apply_restarts_attached_service_before_committing_volume_quota_change() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = volume_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller.clone());

    let first =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("first deploy");

    let mut quota_manifest = volume_manifest();
    let Some(volume) = quota_manifest.volumes.first_mut() else {
        panic!("expected volume");
    };
    volume.quota = "2G".into();
    let quota_plan = resolve_plan(&store, &local_machine_id, &quota_manifest)
        .await
        .expect("quota plan");
    let [service] = quota_plan.services() else {
        panic!("expected one planned service");
    };
    assert_eq!(service.action, crate::model::DeployChangeKind::Replace);

    let second = apply_with_initial_plan(
        &store,
        &factory,
        &local_machine_id,
        &quota_manifest,
        quota_plan,
    )
    .await
    .expect("quota deploy");

    assert_eq!(controller.start_count(), 2);
    let requests = controller.start_requests().await;
    let [_, quota_request] = requests.as_slice() else {
        panic!("expected two start requests");
    };
    let volumes: Vec<VolumeDeclaration> =
        serde_json::from_str(&quota_request.volumes_json).expect("volumes json");
    let [volume] = volumes.as_slice() else {
        panic!("expected one volume declaration");
    };
    assert_eq!(volume.quota, "2G");

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected one committed volume");
    };
    assert_eq!(record.quota, "2G");
    assert_eq!(record.created_by_deploy_id, first.deploy_id);
    assert_eq!(record.last_modified_by_deploy_id, second.deploy_id);
}

#[tokio::test]
async fn apply_executes_volume_move_before_startup_and_commits_target_owner() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-a".into(),
                to_machine: "machine-b".into(),
            },
        }],
        phases: Vec::new(),
    });
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "db",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-1",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");
    backend.reset_counts();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("move plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller.clone());

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("move deploy");

    assert_eq!(result.state, DeployState::Committed);
    assert_eq!(backend.commit_count(), 1);
    assert_eq!(controller.drain_count(), 1);
    assert_eq!(controller.remove_count(), 1);
    assert_eq!(controller.move_count(), 1);
    assert_eq!(controller.start_count(), 1);
    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected moved volume record");
    };
    assert_eq!(record.machine_id, MachineId("machine-b".into()));
    let movements = store
        .list_volume_movements(&manifest.namespace)
        .await
        .expect("list volume movements");
    let [movement] = movements.as_slice() else {
        panic!("expected movement evidence");
    };
    assert_eq!(movement.volume_name, "data");
    assert_eq!(movement.from_machine, MachineId("machine-a".into()));
    assert_eq!(movement.to_machine, MachineId("machine-b".into()));
    assert_eq!(movement.final_machine, MachineId("machine-b".into()));
    assert_eq!(movement.deploy_id, result.deploy_id);
    assert_eq!(movement.commit_deploy_id, result.deploy_id);
    assert_eq!(movement.phase_id, Some(DeployPhaseId("deploy".into())));
    assert_eq!(movement.snapshot_guid, 42);
    assert_eq!(movement.bytes_transferred, 4096);
    let log = controller.operation_log().await;
    let drain_index = log
        .iter()
        .position(|entry| entry.starts_with("drain:"))
        .expect("drain operation logged");
    let remove_index = log
        .iter()
        .position(|entry| entry.starts_with("remove:"))
        .expect("remove operation logged");
    let move_index = log
        .iter()
        .position(|entry| entry.starts_with("move:data:"))
        .expect("move operation logged");
    let start_index = log
        .iter()
        .position(|entry| entry.starts_with("start:db:"))
        .expect("start operation logged");
    assert!(drain_index < remove_index);
    assert!(remove_index < move_index);
    assert!(move_index < start_index);
}

#[tokio::test]
async fn apply_executes_inferred_draining_volume_move_before_startup() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Draining))
        .await
        .expect("mark machine-a draining");
    let local_machine_id = MachineId("local".into());
    let manifest = volume_manifest();
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "db",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-1",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");
    backend.reset_counts();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("inferred move plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller.clone());

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("inferred move deploy");

    assert_eq!(result.state, DeployState::Committed);
    assert_eq!(backend.commit_count(), 1);
    assert_eq!(controller.drain_count(), 1);
    assert_eq!(controller.remove_count(), 1);
    assert_eq!(controller.move_count(), 1);
    assert_eq!(controller.start_count(), 1);
    let move_requests = controller.move_requests().await;
    let [move_request] = move_requests.as_slice() else {
        panic!("expected one move request");
    };
    assert_eq!(move_request.from_machine, MachineId("machine-a".into()));
    assert_eq!(move_request.to_machine, MachineId("machine-b".into()));
    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected moved volume record");
    };
    assert_eq!(record.machine_id, MachineId("machine-b".into()));
}

#[tokio::test]
async fn apply_stops_stale_live_volume_writers_before_move() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-a".into(),
                to_machine: "machine-b".into(),
            },
        }],
        phases: Vec::new(),
    });
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    backend.reset_counts();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("move plan");
    let controller = FakeController::default();
    controller
        .set_inspect_instances(vec![test_instance_status(
            &manifest.namespace,
            "db",
            "stale-slot",
            "machine-a",
            "stale-inst",
            &revision_hash,
        )])
        .await;
    let factory = FakeParticipantClient::new(controller.clone());

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("move deploy");

    assert_eq!(result.state, DeployState::Committed);
    assert_eq!(backend.commit_count(), 1);
    assert_eq!(controller.drain_count(), 1);
    assert_eq!(controller.remove_count(), 1);
    assert_eq!(controller.move_count(), 1);
    assert_eq!(controller.start_count(), 1);
    let log = controller.operation_log().await;
    let drain_index = log
        .iter()
        .position(|entry| entry == "drain:stale-inst")
        .expect("stale drain operation logged");
    let remove_index = log
        .iter()
        .position(|entry| entry == "remove:stale-inst")
        .expect("stale remove operation logged");
    let move_index = log
        .iter()
        .position(|entry| entry.starts_with("move:data:"))
        .expect("move operation logged");
    assert!(drain_index < remove_index);
    assert!(remove_index < move_index);
}

#[tokio::test]
async fn apply_fails_volume_move_before_startup_or_commit() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-a".into(),
                to_machine: "machine-b".into(),
            },
        }],
        phases: Vec::new(),
    });
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "db",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-1",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");
    backend.reset_counts();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("move plan");
    let controller = FakeController {
        fail_move_volume: Some("data".into()),
        ..Default::default()
    };
    let factory = FakeParticipantClient::new(controller.clone());

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("move failure should fail deploy");

    assert_eq!(
        error,
        Error::Operation {
            operation: "fake_move_volume",
            message: "injected move failure for 'data'".into(),
        }
    );
    assert_eq!(backend.commit_count(), 0);
    assert_eq!(backend.deploy_status_write_count(), 2);
    assert_eq!(controller.drain_count(), 1);
    assert_eq!(controller.remove_count(), 1);
    assert_eq!(controller.move_count(), 1);
    assert_eq!(controller.start_count(), 0);
    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected source volume record");
    };
    assert_eq!(record.machine_id, MachineId("machine-a".into()));
    assert!(
        store
            .list_volume_movements(&manifest.namespace)
            .await
            .expect("list volume movements")
            .is_empty()
    );
    let last_update = backend
        .last_deploy_status_write()
        .await
        .expect("failed deploy record should be written");
    assert_eq!(last_update.state, DeployState::Failed);
    assert!(
        last_update
            .summary_json
            .contains("injected move failure for 'data'"),
        "failed deploy summary should mention the move error: {}",
        last_update.summary_json
    );
    assert_default_phase_record(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "failed",
        Some("injected move failure for 'data'"),
    )
    .await;
}

#[tokio::test]
async fn apply_reuses_volume_move_snapshot_when_retrying_after_startup_failure() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-a".into(),
                to_machine: "machine-b".into(),
            },
        }],
        phases: Vec::new(),
    });
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "db",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-1",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");
    backend.reset_counts();

    let controller = FakeController {
        fail_start_service: Some("db".into()),
        ..Default::default()
    };
    let first_client = FakeParticipantClient::new(controller.clone());
    let first_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("first plan");

    apply_with_initial_plan(
        &store,
        &first_client,
        &local_machine_id,
        &manifest,
        first_plan,
    )
    .await
    .expect_err("startup failure should fail first deploy");

    assert_eq!(backend.commit_count(), 0);
    assert_eq!(controller.move_count(), 1);
    assert_eq!(controller.start_count(), 1);
    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected source volume record");
    };
    assert_eq!(record.machine_id, MachineId("machine-a".into()));

    let retry_controller = FakeController::default();
    let retry_client = FakeParticipantClient::new(retry_controller.clone());
    let retry_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("retry plan");

    let result = apply_with_initial_plan(
        &store,
        &retry_client,
        &local_machine_id,
        &manifest,
        retry_plan,
    )
    .await
    .expect("retry deploy");

    assert_eq!(result.state, DeployState::Committed);
    let first_requests = controller.move_requests().await;
    let retry_requests = retry_controller.move_requests().await;
    let [first_request] = first_requests.as_slice() else {
        panic!("expected one first move");
    };
    let [retry_request] = retry_requests.as_slice() else {
        panic!("expected one retry move");
    };
    assert_eq!(first_request.snapshot, retry_request.snapshot);
}

#[tokio::test]
async fn apply_stops_current_volume_writers_even_when_service_is_removed() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = volume_manifest();
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    manifest.services.clear();
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-a".into(),
                to_machine: "machine-b".into(),
            },
        }],
        phases: Vec::new(),
    });
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "db",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-1",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");
    backend.reset_counts();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("move plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller.clone());

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("move deploy");

    assert_eq!(result.state, DeployState::Committed);
    assert_eq!(backend.commit_count(), 1);
    assert_eq!(controller.drain_count(), 1);
    assert_eq!(controller.remove_count(), 1);
    assert_eq!(controller.move_count(), 1);
    assert_eq!(controller.start_count(), 0);
    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected moved volume record");
    };
    assert_eq!(record.machine_id, MachineId("machine-b".into()));
    let log = controller.operation_log().await;
    let drain_index = log
        .iter()
        .position(|entry| entry.starts_with("drain:"))
        .expect("drain operation logged");
    let remove_index = log
        .iter()
        .position(|entry| entry.starts_with("remove:"))
        .expect("remove operation logged");
    let move_index = log
        .iter()
        .position(|entry| entry.starts_with("move:data:"))
        .expect("move operation logged");
    assert!(drain_index < remove_index);
    assert!(remove_index < move_index);
}

#[tokio::test]
async fn apply_does_not_mark_committed_volume_move_failed_after_post_commit_status_error() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-a".into(),
                to_machine: "machine-b".into(),
            },
        }],
        phases: Vec::new(),
    });
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "db",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-1",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");
    backend.reset_counts();
    backend.fail_committed_status_writes_after_first(true);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("move plan");
    let factory = FakeParticipantClient::new(FakeController::default());

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect_err("post-commit status write should fail apply response");

    assert_eq!(backend.commit_count(), 1);
    assert_eq!(backend.deploy_status_write_count(), 3);
    let writes = backend.deploy_status_writes().await;
    assert_eq!(
        writes
            .iter()
            .filter(|record| record.state == DeployState::Failed)
            .count(),
        0
    );
    let committed_attempt = writes
        .last()
        .expect("committed status write should have been attempted");
    assert_eq!(committed_attempt.state, DeployState::Committed);
    let committed_record = store
        .get_deploy(&committed_attempt.deploy_id)
        .await
        .expect("get deploy")
        .expect("committed deploy record");
    assert_eq!(committed_record.state, DeployState::Committed);
    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected moved volume record");
    };
    assert_eq!(record.machine_id, MachineId("machine-b".into()));
    let movements = store
        .list_volume_movements(&manifest.namespace)
        .await
        .expect("list volume movements");
    assert_eq!(movements.len(), 1);
}

#[tokio::test]
async fn apply_rejects_volume_move_when_target_loses_eligibility_before_mutation() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-a".into(),
                to_machine: "machine-b".into(),
            },
        }],
        phases: Vec::new(),
    });
    seed_volume(&store, &manifest.namespace, "data", "machine-a").await;
    backend.reset_counts();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial move plan");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Standby))
        .await
        .expect("target becomes standby");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller.clone());

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("target eligibility drift should fail before mutation");

    assert_eq!(
        error,
        Error::Deploy(DeployError::VolumeMoveTargetIneligible {
            volume: "data".into(),
            machine_id: "machine-b".into()
        })
    );
    assert_eq!(backend.commit_count(), 0);
    assert_eq!(backend.deploy_status_write_count(), 0);
    assert_eq!(controller.start_count(), 0);
}

#[tokio::test]
async fn apply_rejects_unsupported_volume_move_before_probe_or_inspect() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-a".into(),
                to_machine: "machine-b".into(),
            },
        }],
        phases: Vec::new(),
    });
    seed_volume(&store, &manifest.namespace, "data", "machine-a").await;
    let participant = UnsupportedParticipantClient::default();
    let prober = FailingParticipantProbe {
        machine_id: MachineId("machine-b".into()),
    };

    let error = apply_with_certificate_coordination(
        &store,
        &participant,
        &local_machine_id,
        &manifest,
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &prober,
    )
    .await
    .expect_err("unsupported move should fail before participant work");

    assert_eq!(
        error,
        Error::Deploy(DeployError::VolumeMoveExecutionUnsupported {
            volume: "data".into()
        })
    );
    assert_eq!(participant.inspect_count(), 0);
}

#[tokio::test]
async fn apply_allows_unsupported_volume_move_client_when_plan_has_no_moves() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.27",
    )]);
    let participant = UnsupportedParticipantClient::default();
    let prober = FailingParticipantProbe {
        machine_id: MachineId("unused".into()),
    };

    let error = apply_with_certificate_coordination(
        &store,
        &participant,
        &local_machine_id,
        &manifest,
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &prober,
    )
    .await
    .expect_err("non-move deploy should reach participant startup");

    assert_eq!(
        error,
        Error::Operation {
            operation: "unsupported_participant",
            message: "start".into()
        }
    );
    assert_eq!(participant.inspect_count(), 1);
}

#[tokio::test]
async fn apply_deletes_volume_records_removed_from_manifest() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = volume_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller);

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect("first deploy");

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected seeded volume");
    };
    assert_eq!(record.volume_name, "data");

    let next_manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.28",
    )]);
    let next_plan = resolve_plan(&store, &local_machine_id, &next_manifest)
        .await
        .expect("removal plan");
    apply_with_initial_plan(
        &store,
        &factory,
        &local_machine_id,
        &next_manifest,
        next_plan,
    )
    .await
    .expect("remove volume deploy");

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes after removal");
    assert!(
        records.is_empty(),
        "expected volume record removed: {records:?}"
    );
}

#[tokio::test]
async fn apply_keeps_retained_volume_when_attached_service_is_removed() {
    // Regression: a service that mounts a volume can be removed from the manifest
    // while the volume itself is retained. The VolumeRecord must stay in the
    // store, but its attached_services must drop the now-deleted service so it
    // doesn't keep pointing at a name that no longer exists.
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = volume_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller);

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect("first deploy");

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected seeded volume");
    };
    assert_eq!(record.volume_name, "data");
    assert_eq!(record.attached_services, vec!["db"]);

    // Replace `db` (which mounted `data`) with an unrelated `api` service while
    // keeping the volume declared.
    let mut next_manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.28",
    )]);
    next_manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    let next_plan = resolve_plan(&store, &local_machine_id, &next_manifest)
        .await
        .expect("removal plan");
    apply_with_initial_plan(
        &store,
        &factory,
        &local_machine_id,
        &next_manifest,
        next_plan,
    )
    .await
    .expect("redeploy without db");

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes after service removal");
    let [record] = records.as_slice() else {
        panic!("expected volume retained, got: {records:?}");
    };
    assert_eq!(record.volume_name, "data");
    assert!(
        record.attached_services.is_empty(),
        "expected attached_services cleared, got: {:?}",
        record.attached_services
    );
}

#[tokio::test]
async fn apply_commits_unattached_volume_declarations_without_service_restart() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.27",
    )]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller.clone());

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect("deploy");

    assert_eq!(controller.start_count(), 1);
    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected one committed volume");
    };
    assert_eq!(record.volume_name, "data");
    assert!(record.attached_services.is_empty());
}

#[tokio::test]
async fn apply_preserves_unattached_volume_record_on_unchanged_redeploy() {
    // The redeploy-with-attached-service skip path is covered above. Pin the
    // analogous skip behavior for a volume with no attached service: declared
    // in the manifest, no service mounts it, last_modified_by_deploy_id stays
    // tied to the first deploy after a no-op redeploy.
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.27",
    )]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));

    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller);

    let first =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("first deploy");

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes after first apply");
    let [record] = records.as_slice() else {
        panic!("expected one committed volume, got: {records:?}");
    };
    assert!(record.attached_services.is_empty());
    assert_eq!(record.last_modified_by_deploy_id, first.deploy_id);

    let second_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("second plan");
    let second =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, second_plan)
            .await
            .expect("second deploy");

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes after redeploy");
    let [record] = records.as_slice() else {
        panic!("expected one committed volume after redeploy, got: {records:?}");
    };
    assert!(record.attached_services.is_empty());
    assert_eq!(
        record.last_modified_by_deploy_id, first.deploy_id,
        "unchanged unattached volume should not be rewritten"
    );
    assert_ne!(record.last_modified_by_deploy_id, second.deploy_id);
}

#[tokio::test]
async fn resolve_plan_rejects_existing_volume_quota_shrink() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = volume_manifest();
    seed_volume_with(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "2G",
        "0750",
        "999:999",
    )
    .await;

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("quota shrink should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::VolumeQuotaShrink {
            volume: "data".into()
        })
    );
}

#[tokio::test]
async fn resolve_plan_rejects_existing_volume_scope_mode_or_owner_changes() {
    let local_machine_id = MachineId("local".into());

    for (field, manifest, expected) in [
        (
            "scope",
            {
                let mut manifest = volume_manifest();
                let Some(volume) = manifest.volumes.first_mut() else {
                    panic!("expected volume");
                };
                volume.scope = VolumeScope::Shared;
                manifest
            },
            Error::Deploy(DeployError::VolumeScopeChange {
                volume: "data".into(),
            }),
        ),
        (
            "mode",
            {
                let mut manifest = volume_manifest();
                let Some(volume) = manifest.volumes.first_mut() else {
                    panic!("expected volume");
                };
                volume.mode = "0700".into();
                manifest
            },
            Error::Deploy(DeployError::VolumeModeChange {
                volume: "data".into(),
            }),
        ),
        (
            "owner",
            {
                let mut manifest = volume_manifest();
                let Some(volume) = manifest.volumes.first_mut() else {
                    panic!("expected volume");
                };
                volume.owner = "1000:1000".into();
                manifest
            },
            Error::Deploy(DeployError::VolumeOwnerChange {
                volume: "data".into(),
            }),
        ),
    ] {
        let store = seeded_store_with_machines(&["machine-a"]).await;
        seed_volume(&store, &manifest.namespace, "data", "machine-a").await;

        let error = match resolve_plan(&store, &local_machine_id, &manifest).await {
            Ok(_) => panic!("{field} change should fail"),
            Err(error) => error,
        };

        assert_eq!(error, expected, "{field} error should be typed");
    }
}

#[tokio::test]
async fn resolve_plan_rejects_invalid_stored_volume_quota_with_structured_error() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = volume_manifest();
    seed_volume_with(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "bogus",
        "0750",
        "999:999",
    )
    .await;

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("invalid stored quota should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::VolumeQuotaInvalid {
            volume: "data".into(),
            quota_kind: "current",
            message: "unsupported quota suffix in 'bogus'".into()
        })
    );
}

#[tokio::test]
async fn resolve_plan_global_service_targets_enabled_machines_in_order() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Global,
        "nginx:1.27",
    )]);

    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Active))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-c", MachineLifecycle::Draining))
        .await
        .expect("seed machine-c");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [resolution] = plan.services() else {
        panic!("expected one service resolution");
    };
    let desired = resolution
        .slots
        .iter()
        .map(|slot| (slot.slot_id.clone(), slot.machine_id.clone()))
        .collect::<Vec<_>>();

    assert_eq!(
        desired,
        vec![
            (
                SlotId("slot-machine-a".into()),
                MachineId("machine-a".into())
            ),
            (
                SlotId("slot-machine-b".into()),
                MachineId("machine-b".into())
            ),
        ]
    );
}

#[tokio::test]
async fn resolve_plan_global_service_targets_home_and_compute_regions_only() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Global,
        "nginx:1.27",
    )]);

    for machine in [
        test_machine_in_region("home", MachineLifecycle::Active, RegionRole::HomeData),
        test_machine_in_region("compute", MachineLifecycle::Active, RegionRole::Compute),
        test_machine_in_region(
            "region-draining",
            MachineLifecycle::Active,
            RegionRole::Draining,
        ),
        test_machine_in_region(
            "region-disabled",
            MachineLifecycle::Active,
            RegionRole::Disabled,
        ),
    ] {
        store
            .upsert_self_machine(&machine)
            .await
            .expect("seed machine");
    }

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [resolution] = plan.services() else {
        panic!("expected one service resolution");
    };
    let desired = resolution
        .slots
        .iter()
        .map(|slot| slot.machine_id.clone())
        .collect::<Vec<_>>();

    assert_eq!(
        desired,
        vec![MachineId("compute".into()), MachineId("home".into())]
    );
}

#[tokio::test]
async fn resolve_plan_fails_when_no_stored_machine_is_eligible_for_new_placement() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.27",
    )]);

    for machine in [
        test_machine_in_region(
            "region-draining",
            MachineLifecycle::Active,
            RegionRole::Draining,
        ),
        test_machine_in_region(
            "region-disabled",
            MachineLifecycle::Active,
            RegionRole::Disabled,
        ),
    ] {
        store
            .upsert_self_machine(&machine)
            .await
            .expect("seed machine");
    }

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("no eligible target should fail");
    assert!(matches!(
        error,
        Error::Deploy(DeployError::NoEligiblePlacementTargets)
    ));
}

#[tokio::test]
async fn resolve_plan_fails_new_volume_when_no_machine_is_eligible() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let mut service = test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17");
    service.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![service]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));

    for machine in [
        test_machine_in_region(
            "region-draining",
            MachineLifecycle::Active,
            RegionRole::Draining,
        ),
        test_machine_in_region(
            "region-disabled",
            MachineLifecycle::Active,
            RegionRole::Disabled,
        ),
    ] {
        store
            .upsert_self_machine(&machine)
            .await
            .expect("seed machine");
    }

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("new volume with no eligible target should fail");
    assert!(matches!(
        error,
        Error::Deploy(DeployError::NoEligiblePlacementTargets)
    ));
}

#[tokio::test]
async fn resolve_plan_allows_removal_only_when_no_new_placement_target_exists() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(Vec::new());

    store
        .upsert_self_machine(&test_machine_in_region(
            "region-draining",
            MachineLifecycle::Active,
            RegionRole::Draining,
        ))
        .await
        .expect("seed machine");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "old-api",
            "rev-old",
            vec![test_slot(
                "slot-0001",
                "region-draining",
                "inst-old",
                "rev-old",
            )],
        ))
        .await
        .expect("seed old-api release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("removal-only plan should not need a new placement target");

    let [removed] = plan.services() else {
        panic!("expected one removed service");
    };
    assert_eq!(removed.service, "old-api");
    assert_eq!(removed.action, DeployChangeKind::Remove);
}

#[tokio::test]
async fn resolve_plan_includes_removed_service_participants() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.27",
    )]);
    let [current_spec] = manifest.services.as_slice() else {
        panic!("expected one manifest service");
    };
    let current_revision_hash = current_spec.revision_hash().expect("current revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Active))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Draining))
        .await
        .expect("seed machine-b");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "old-api",
            "rev-old",
            vec![test_slot("slot-0001", "machine-b", "inst-old", "rev-old")],
        ))
        .await
        .expect("seed old-api release");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &current_revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-current",
                &current_revision_hash,
            )],
        ))
        .await
        .expect("seed api release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    assert!(plan.participants().contains(&MachineId("machine-b".into())));
}

#[tokio::test]
async fn resolve_plan_fingerprint_is_stable_across_release_insert_order() {
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![
        test_service_spec("api", Placement::Replicated { count: 1 }, "nginx:1.27"),
        test_service_spec("worker", Placement::Replicated { count: 1 }, "busybox:1.0"),
    ]);
    let [api_spec, worker_spec] = manifest.services.as_slice() else {
        panic!("expected two services");
    };
    let api_revision = api_spec.revision_hash().expect("api revision");
    let worker_revision = worker_spec.revision_hash().expect("worker revision");

    let store_a = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    store_a
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &api_revision,
            vec![test_slot("slot-0001", "machine-a", "inst-a", &api_revision)],
        ))
        .await
        .expect("api release");
    store_a
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "worker",
            &worker_revision,
            vec![test_slot(
                "slot-0001",
                "machine-b",
                "inst-b",
                &worker_revision,
            )],
        ))
        .await
        .expect("worker release");

    let store_b = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    store_b
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "worker",
            &worker_revision,
            vec![test_slot(
                "slot-0001",
                "machine-b",
                "inst-b",
                &worker_revision,
            )],
        ))
        .await
        .expect("worker release");
    store_b
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &api_revision,
            vec![test_slot("slot-0001", "machine-a", "inst-a", &api_revision)],
        ))
        .await
        .expect("api release");

    let plan_a = resolve_plan(&store_a, &local_machine_id, &manifest)
        .await
        .expect("plan a");
    let plan_b = resolve_plan(&store_b, &local_machine_id, &manifest)
        .await
        .expect("plan b");

    assert_eq!(plan_a.fingerprint(), plan_b.fingerprint());
}

#[tokio::test]
async fn resolve_plan_includes_branch_source_preview_evidence() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let source_namespace = Namespace("prod".into());
    let source_spec = test_service_spec("web", Placement::Replicated { count: 1 }, "nginx:1.27");
    let source_revision_hash = source_spec.revision_hash().expect("source revision hash");
    seed_committed_service_release(&store, &source_namespace, source_spec).await;

    let mut manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::Replicated { count: 1 },
        "example/web:pr-39",
    )]);
    manifest.namespace = Namespace("pr-39".into());
    manifest.intent = Some(DeployIntent {
        services: vec![ServiceIntentHint {
            service: "web".into(),
            intent: ServiceIntent::Branch {
                source_namespace: source_namespace.clone(),
                source_service: "web".into(),
            },
        }],
        volumes: Vec::new(),
        phases: Vec::new(),
    });

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve branch plan");
    let preview = plan.to_preview(Vec::new());

    assert_eq!(preview.namespace, Namespace("pr-39".into()));
    assert_eq!(preview.services[0].action, DeployChangeKind::Create);
    assert_eq!(preview.service_branch_sources.len(), 1);
    assert_eq!(preview.service_branch_sources[0].service, "web");
    assert_eq!(
        preview.service_branch_sources[0].source_revision_hash,
        source_revision_hash
    );
    let [phase] = preview.phases.as_slice() else {
        panic!("expected one default deploy phase");
    };
    assert!(matches!(
        phase.work.as_slice(),
        [DeployPhaseWork::Service { service, action }]
            if service == "web" && *action == DeployChangeKind::Create
    ));
    assert_eq!(
        plan.fingerprint().services[0]
            .branch_source
            .as_ref()
            .map(|source| source.source_revision_hash.as_str()),
        Some(source_revision_hash.as_str())
    );
}

#[tokio::test]
async fn resolve_plan_rejects_missing_branch_source_release() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::Replicated { count: 1 },
        "example/web:pr-39",
    )]);
    manifest.namespace = Namespace("pr-39".into());
    manifest.intent = Some(DeployIntent {
        services: vec![ServiceIntentHint {
            service: "web".into(),
            intent: ServiceIntent::Branch {
                source_namespace: Namespace("prod".into()),
                source_service: "web".into(),
            },
        }],
        volumes: Vec::new(),
        phases: Vec::new(),
    });

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("missing source release should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::BranchSourceMissingRelease {
            namespace: "prod".into(),
            service: "web".into()
        })
    );
}

#[tokio::test]
async fn resolve_plan_rejects_branch_source_missing_revision() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let source_namespace = Namespace("prod".into());
    store
        .upsert_service_release(&test_release(
            &source_namespace,
            "web",
            "missing-source-rev",
            Vec::new(),
        ))
        .await
        .expect("seed source release without revision");
    let mut manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::Replicated { count: 1 },
        "example/web:pr-39",
    )]);
    manifest.namespace = Namespace("pr-39".into());
    manifest.intent = Some(DeployIntent {
        services: vec![ServiceIntentHint {
            service: "web".into(),
            intent: ServiceIntent::Branch {
                source_namespace: source_namespace.clone(),
                source_service: "web".into(),
            },
        }],
        volumes: Vec::new(),
        phases: Vec::new(),
    });

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("missing source revision should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::BranchSourceMissingRevision {
            namespace: "prod".into(),
            service: "web".into(),
            revision_hash: "missing-source-rev".into()
        })
    );
}

#[tokio::test]
async fn resolve_plan_rejects_undecodable_branch_source_revision() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let source_namespace = Namespace("prod".into());
    store
        .commit_deploy(&DeployCommit {
            namespace: source_namespace.clone(),
            revisions: vec![ServiceRevisionRecord {
                namespace: source_namespace.clone(),
                service: "web".into(),
                revision_hash: "source-rev".into(),
                spec_json: "{not-json".into(),
                created_by: MachineId("seed".into()),
                created_at: 0,
            }],
            removed_services: Vec::new(),
            removed_volumes: Vec::new(),
            branch_lineage: Vec::new(),
            volume_movements: Vec::new(),
            volume_branches: Vec::new(),
            phase_commits: Vec::new(),
            releases: vec![test_release(
                &source_namespace,
                "web",
                "source-rev",
                Vec::new(),
            )],
            volumes: Vec::new(),
            deploy: test_deploy_record(&source_namespace, "seed-deploy"),
        })
        .await
        .expect("seed undecodable source revision");
    let mut manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::Replicated { count: 1 },
        "example/web:pr-39",
    )]);
    manifest.namespace = Namespace("pr-39".into());
    manifest.intent = Some(DeployIntent {
        services: vec![ServiceIntentHint {
            service: "web".into(),
            intent: ServiceIntent::Branch {
                source_namespace: source_namespace.clone(),
                source_service: "web".into(),
            },
        }],
        volumes: Vec::new(),
        phases: Vec::new(),
    });

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("undecodable source revision should fail");

    assert!(matches!(
        error,
        Error::Deploy(DeployError::BranchSourceSpecDecode {
            namespace,
            service,
            ..
        }) if namespace == "prod" && service == "web"
    ));
}

#[tokio::test]
async fn resolve_plan_rejects_branch_source_same_as_target() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::Replicated { count: 1 },
        "example/web:pr-39",
    )]);
    manifest.namespace = Namespace("pr-39".into());
    manifest.intent = Some(DeployIntent {
        services: vec![ServiceIntentHint {
            service: "web".into(),
            intent: ServiceIntent::Branch {
                source_namespace: Namespace("pr-39".into()),
                source_service: "web".into(),
            },
        }],
        volumes: Vec::new(),
        phases: Vec::new(),
    });

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("same-target branch source should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::BranchSourceIsTarget {
            namespace: "pr-39".into(),
            service: "web".into()
        })
    );
}

#[tokio::test]
async fn ensure_plan_stable_rejects_branch_source_revision_drift() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let source_namespace = Namespace("prod".into());
    seed_committed_service_release(
        &store,
        &source_namespace,
        test_service_spec("web", Placement::Replicated { count: 1 }, "nginx:1.27"),
    )
    .await;

    let mut manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::Replicated { count: 1 },
        "example/web:pr-39",
    )]);
    manifest.namespace = Namespace("pr-39".into());
    manifest.intent = Some(DeployIntent {
        services: vec![ServiceIntentHint {
            service: "web".into(),
            intent: ServiceIntent::Branch {
                source_namespace: source_namespace.clone(),
                source_service: "web".into(),
            },
        }],
        volumes: Vec::new(),
        phases: Vec::new(),
    });

    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial branch plan");
    seed_committed_service_release(
        &store,
        &source_namespace,
        test_service_spec("web", Placement::Replicated { count: 1 }, "nginx:1.28"),
    )
    .await;
    let final_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("final branch plan");

    let error = ensure_plan_stable(&initial_plan.fingerprint(), &final_plan.fingerprint())
        .expect_err("source revision drift should fail");
    assert_eq!(error, Error::Deploy(DeployError::ExecutionPlanChanged));
}

#[tokio::test]
async fn participant_set_inspects_participants_in_parallel_for_noop_plan() {
    let store = seeded_store_with_machines(&[
        "machine-a",
        "machine-b",
        "machine-c",
        "machine-d",
        "machine-e",
    ])
    .await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Global,
        "nginx:1.27",
    )]);
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &revision_hash,
            vec![
                test_slot("slot-machine-a", "machine-a", "inst-a", &revision_hash),
                test_slot("slot-machine-b", "machine-b", "inst-b", &revision_hash),
                test_slot("slot-machine-c", "machine-c", "inst-c", &revision_hash),
                test_slot("slot-machine-d", "machine-d", "inst-d", &revision_hash),
                test_slot("slot-machine-e", "machine-e", "inst-e", &revision_hash),
            ],
        ))
        .await
        .expect("seed release");

    let controller = FakeController {
        open_delay: Duration::from_millis(25),
        start_delay: Duration::from_millis(5),
        ..Default::default()
    };
    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");
    let factory = FakeParticipantClient::new(controller.clone());
    let deploy_id = DeployId("deploy-open".into());

    let (_participants, _events) =
        ParticipantSet::inspect(&factory, &plan, &local_machine_id, &deploy_id)
            .await
            .expect("inspect participants");

    assert_eq!(controller.max_open_seen(), 5);
    assert_eq!(controller.start_count(), 0);
}

#[tokio::test]
async fn phase_startup_uses_one_worker_per_machine_but_parallel_across_machines() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![
        test_service_spec("api", Placement::Replicated { count: 2 }, "nginx:1.27"),
        test_service_spec("worker", Placement::Replicated { count: 2 }, "busybox:1.0"),
    ]);

    let controller = FakeController {
        start_delay: Duration::from_millis(40),
        ..Default::default()
    };
    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");
    let factory = FakeParticipantClient::new(controller.clone());
    let deploy_id = DeployId("deploy-phase".into());
    let (participants, _events) =
        ParticipantSet::inspect(&factory, &plan, &local_machine_id, &deploy_id)
            .await
            .expect("inspect participants");
    let startup = run_phase_startup(&store, &factory, &participants, &plan)
        .await
        .expect("run startup");

    assert_eq!(startup.started.len(), 4);
    assert_eq!(controller.start_count(), 4);
    assert!(controller.max_global_start_seen() >= 2);
    assert_eq!(controller.max_machine_start_seen("machine-a"), 1);
    assert_eq!(controller.max_machine_start_seen("machine-b"), 1);
}

#[tokio::test]
async fn phase_startup_stops_scheduling_machine_queues_after_failure() {
    let machine_names = (0..65)
        .map(|index| format!("machine-{index:02}"))
        .collect::<Vec<_>>();
    let machine_refs = machine_names.iter().map(String::as_str).collect::<Vec<_>>();
    let store = seeded_store_with_machines(&machine_refs).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Global,
        "nginx:1.27",
    )]);
    let controller = FakeController {
        fail_start_service: Some("api".into()),
        ..Default::default()
    };
    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");
    let factory = FakeParticipantClient::new(controller.clone());
    let deploy_id = DeployId("deploy-phase".into());
    let (participants, _events) =
        ParticipantSet::inspect(&factory, &plan, &local_machine_id, &deploy_id)
            .await
            .expect("inspect participants");

    run_phase_startup(&store, &factory, &participants, &plan)
        .await
        .expect_err("startup failure should fail phase");

    assert_eq!(controller.start_count(), 64);
}

#[tokio::test]
async fn run_phase_startup_waits_for_previous_phase_before_next_phase() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![
        test_service_spec("api", Placement::Replicated { count: 2 }, "nginx:1.27"),
        test_service_spec("worker", Placement::Replicated { count: 2 }, "busybox:1.0"),
    ]);
    let mut plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");
    let [first, second] = plan.services_mut() else {
        panic!("expected two planned services");
    };
    first.phase = Some(0);
    second.phase = Some(1);

    let controller = FakeController {
        start_delay: Duration::from_millis(20),
        ..Default::default()
    };
    let factory = FakeParticipantClient::new(controller.clone());
    let deploy_id = DeployId("deploy-test".into());
    let (participants, _events) =
        ParticipantSet::inspect(&factory, &plan, &local_machine_id, &deploy_id)
            .await
            .expect("inspect participants");

    let startup = run_phase_startup(&store, &factory, &participants, &plan)
        .await
        .expect("run phases");

    assert_eq!(startup.started.len(), 4);
    let log = controller.start_log().await;
    let first_worker = log
        .iter()
        .position(|entry| entry.contains("worker"))
        .expect("worker start present");
    let last_api = log
        .iter()
        .rposition(|entry| entry.contains("api"))
        .expect("api start present");
    assert!(last_api < first_worker);
}

#[tokio::test]
async fn ensure_plan_stable_rejects_post_lock_drift() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Global,
        "nginx:1.27",
    )]);
    let drift_manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Global,
        "nginx:1.28",
    )]);
    let [current_spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let current_revision = current_spec.revision_hash().expect("current revision");
    let [drift_spec] = drift_manifest.services.as_slice() else {
        panic!("expected one drift service");
    };
    let drift_revision = drift_spec.revision_hash().expect("drift revision");

    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &current_revision,
            vec![
                test_slot("slot-machine-a", "machine-a", "inst-a", &current_revision),
                test_slot("slot-machine-b", "machine-b", "inst-b", &current_revision),
            ],
        ))
        .await
        .expect("seed release");

    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &drift_revision,
            vec![
                test_slot("slot-machine-a", "machine-a", "inst-a2", &drift_revision),
                test_slot("slot-machine-b", "machine-b", "inst-b2", &drift_revision),
            ],
        ))
        .await
        .expect("drift release");
    let final_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("final plan");

    let error = ensure_plan_stable(&initial_plan.fingerprint(), &final_plan.fingerprint())
        .expect_err("plan drift should fail");
    assert_eq!(error, Error::Deploy(DeployError::ExecutionPlanChanged));
}

#[tokio::test]
async fn preview_rejects_duplicate_hostname_in_final_plan() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![
        http_route_service_spec("api", "api.example.com"),
        http_route_service_spec("web", "API.EXAMPLE.COM."),
    ]);

    let error = preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect_err("duplicate hostname should fail preview");

    assert_eq!(
        error,
        Error::Deploy(DeployError::HostnameDeclaredByMultipleServices {
            hostname: "api.example.com".into(),
            first_namespace: "test".into(),
            first_service: "api".into(),
            second_namespace: "test".into(),
            second_service: "web".into()
        })
    );
}

#[tokio::test]
async fn preview_rejects_hostname_owned_by_another_namespace() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    seed_committed_http_release(&store, "prod", "api", "api.example.com").await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![http_route_service_spec("web", "api.example.com")]);

    let error = preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect_err("cross-namespace hostname conflict should fail preview");

    assert_eq!(
        error,
        Error::Deploy(DeployError::HostnameAlreadyOwned {
            hostname: "api.example.com".into(),
            owner_namespace: "prod".into(),
            owner_service: "api".into(),
            request_namespace: "test".into(),
            request_service: "web".into()
        })
    );
}

#[tokio::test]
async fn apply_rejects_hostname_owned_by_another_namespace_before_commit() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    seed_committed_http_release(&store, "prod", "api", "api.example.com").await;
    backend.reset_counts();
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![http_route_service_spec("web", "api.example.com")]);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController::default());

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("cross-namespace hostname conflict should fail apply");

    assert_eq!(
        error,
        Error::Deploy(DeployError::HostnameAlreadyOwned {
            hostname: "api.example.com".into(),
            owner_namespace: "prod".into(),
            owner_service: "api".into(),
            request_namespace: "test".into(),
            request_service: "web".into()
        })
    );
    assert_eq!(backend.commit_count(), 0);
}

#[tokio::test]
async fn apply_rejects_unreachable_participant_before_inspect_or_commit() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 2 },
        "nginx:1.27",
    )]);
    let controller = FakeController::default();
    let participant_client = FakeParticipantClient::new(controller.clone());
    let prober = FailingParticipantProbe {
        machine_id: MachineId("machine-b".into()),
    };

    let error = apply_with_certificate_coordination(
        &store,
        &participant_client,
        &local_machine_id,
        &manifest,
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &prober,
    )
    .await
    .expect_err("unreachable participant should block deploy");

    assert_eq!(
        error,
        Error::Deploy(DeployError::ParticipantsUnreachable {
            unreachable_count: 1,
            participant_count: 2,
            machine_ids: vec!["machine-b".into()]
        })
    );
    assert_eq!(backend.deploy_status_write_count(), 0);
    assert_eq!(backend.commit_count(), 0);
    assert_eq!(controller.max_open_seen(), 0);
    assert_eq!(controller.start_count(), 0);
}

#[tokio::test]
async fn preview_allows_hostname_reuse_within_same_namespace() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    seed_committed_http_release(&store, "test", "api", "api.example.com").await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![http_route_service_spec("api", "api.example.com")]);

    preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect("same-namespace replacement should be valid");
}

#[tokio::test]
async fn preview_allows_hostname_move_within_same_namespace() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    seed_committed_http_release(&store, "test", "api", "api.example.com").await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![http_route_service_spec("web", "api.example.com")]);

    preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect("same-namespace ownership move should be valid");
}

#[tokio::test]
async fn preview_surfaces_unreachable_participants_without_mutating_deploy_state() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 2 },
        "nginx:1.27",
    )]);
    backend.reset_counts();
    let prober = FailingParticipantProbe {
        machine_id: MachineId("machine-b".into()),
    };

    let preview = preview(&store, &local_machine_id, &manifest, &prober)
        .await
        .expect("preview should surface reachability as warnings");

    assert!(preview.warnings.iter().any(|warning| {
        warning.contains("machine-b")
            && warning.contains("timeout")
            && warning.contains("injected probe timeout")
    }));
    assert_eq!(backend.deploy_status_write_count(), 0);
    assert_eq!(backend.commit_count(), 0);
}

#[tokio::test]
async fn apply_with_initial_plan_does_not_commit_when_participant_inspect_fails() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 2 },
        "nginx:1.27",
    )]);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController {
        fail_open_machine: Some("machine-b".into()),
        ..Default::default()
    });

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("apply should fail");

    assert!(error.to_string().contains("injected open failure"));
    assert_eq!(backend.commit_count(), 0);
}

#[tokio::test]
async fn apply_with_initial_plan_does_not_commit_when_start_candidate_fails() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 2 },
        "nginx:1.27",
    )]);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController {
        fail_start_service: Some("api".into()),
        ..Default::default()
    });

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("apply should fail");

    assert!(error.to_string().contains("injected start failure"));
    assert_eq!(backend.commit_count(), 0);
    let releases = store
        .list_service_releases(&manifest.namespace)
        .await
        .expect("list releases");
    assert!(releases.is_empty());
    let revisions = store
        .list_deploy_revisions(&manifest.namespace)
        .await
        .expect("list deploy revisions");
    assert!(
        revisions.is_empty(),
        "failed deploy must not publish uncommitted revision facts"
    );
    let last_update = backend
        .last_deploy_status_write()
        .await
        .expect("failed deploy record should be written");
    assert_eq!(last_update.state, DeployState::Failed);
    assert!(last_update.finished_at.is_some());
    assert!(
        last_update
            .summary_json
            .contains("injected start failure for 'api'"),
        "failed deploy summary should mention the apply error: {}",
        last_update.summary_json
    );
    assert_default_phase_record(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "failed",
        Some("injected start failure for 'api'"),
    )
    .await;
}

#[tokio::test]
async fn apply_marks_default_phase_failed_when_commit_fails_after_phase_work() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.27",
    )]);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    backend.fail_commit(true);
    let factory = FakeParticipantClient::new(FakeController::default());

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("commit failure should fail deploy");

    assert!(error.to_string().contains("injected commit failure"));
    assert_eq!(backend.commit_count(), 1);
    let last_update = backend
        .last_deploy_status_write()
        .await
        .expect("failed deploy record should be written");
    assert_eq!(last_update.state, DeployState::Failed);
    assert_default_phase_record(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "failed",
        Some("injected commit failure"),
    )
    .await;
}

#[tokio::test]
async fn apply_records_committed_status_when_phase_success_evidence_fails() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.27",
    )]);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    backend.fail_succeeded_phase_upsert(true);
    let factory = FakeParticipantClient::new(FakeController::default());

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("phase success evidence failure should not fail committed deploy");

    assert_eq!(result.state, DeployState::Committed);
    assert_eq!(backend.commit_count(), 1);
    let writes = backend.deploy_status_writes().await;
    assert!(
        writes
            .iter()
            .any(|record| record.state == DeployState::Committed),
        "committed deploy status should be written even when phase evidence fails"
    );
    assert!(
        writes
            .iter()
            .all(|record| record.state != DeployState::Failed),
        "phase evidence failure must not mark committed deploy failed"
    );
    let committed_record = store
        .get_deploy(&result.deploy_id)
        .await
        .expect("get deploy")
        .expect("committed deploy record");
    assert_eq!(committed_record.state, DeployState::Committed);
    assert_default_phase_record(
        &store,
        &manifest.namespace,
        &result.deploy_id,
        "succeeded",
        None,
    )
    .await;
    let phase = store
        .get_deploy_phase(
            &manifest.namespace,
            &result.deploy_id,
            &DeployPhaseId("deploy".into()),
        )
        .await
        .expect("get phase")
        .expect("phase");
    assert_eq!(phase.commit_deploy_id, Some(result.deploy_id));
}

#[tokio::test]
async fn apply_marks_phase_succeeded_when_first_committed_status_write_fails() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.27",
    )]);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    backend.fail_committed_status_writes(true);
    let factory = FakeParticipantClient::new(FakeController::default());

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("committed status write failure should fail response");

    assert!(
        error
            .to_string()
            .contains("injected committed status failure")
    );
    assert_eq!(backend.commit_count(), 1);
    let writes = backend.deploy_status_writes().await;
    assert!(
        writes
            .iter()
            .all(|record| record.state != DeployState::Failed),
        "post-commit status failure must not mark committed deploy failed"
    );
    let committed_attempt = writes
        .iter()
        .find(|record| record.state == DeployState::Committed)
        .expect("committed status write should have been attempted");
    assert_default_phase_record(
        &store,
        &manifest.namespace,
        &committed_attempt.deploy_id,
        "succeeded",
        None,
    )
    .await;
}

#[tokio::test]
async fn resolve_plan_builds_manifest_phase_work_in_dependency_order() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = test_manifest(vec![
        test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17"),
        test_service_spec("web", Placement::Replicated { count: 1 }, "nginx:1.27"),
    ]);
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: Vec::new(),
        phases: vec![
            DeployPhaseIntent {
                phase_id: "web".into(),
                name: Some("Web".into()),
                after: vec!["db".into()],
                services: vec!["web".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
            DeployPhaseIntent {
                phase_id: "db".into(),
                name: Some("Database".into()),
                after: Vec::new(),
                services: vec!["db".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::Checkpoint,
                rollback_policy: DeployPhaseRollbackPolicy::ForwardOnly,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
        ],
    });

    let preview = preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect("preview");

    let [db_phase, web_phase] = preview.phases.as_slice() else {
        panic!("expected two phases, got {:?}", preview.phases);
    };
    assert_eq!(db_phase.phase_id, DeployPhaseId("db".into()));
    assert_eq!(db_phase.order, 0);
    assert_eq!(db_phase.commit_policy, DeployPhaseCommitPolicy::Checkpoint);
    assert!(db_phase.after.is_empty());
    assert_eq!(
        db_phase.work.as_slice(),
        &[DeployPhaseWork::Service {
            service: "db".into(),
            action: DeployChangeKind::Create,
        }]
    );
    assert_eq!(web_phase.phase_id, DeployPhaseId("web".into()));
    assert_eq!(web_phase.order, 1);
    assert_eq!(web_phase.after, vec![DeployPhaseId("db".into())]);
    assert_eq!(
        web_phase.commit_policy,
        DeployPhaseCommitPolicy::EndOfDeploy
    );
    assert_eq!(
        web_phase.work.as_slice(),
        &[DeployPhaseWork::Service {
            service: "web".into(),
            action: DeployChangeKind::Create,
        }]
    );
}

#[tokio::test]
async fn apply_checkpoint_phase_commits_before_final_phase() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = checkpointed_db_web_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController::default());

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("apply");

    assert_eq!(result.state, DeployState::Committed);
    assert_eq!(backend.commit_count(), 2);
    let writes = backend.deploy_status_writes().await;
    assert!(
        writes
            .iter()
            .any(|record| record.deploy_id == result.deploy_id
                && record.state == DeployState::CheckpointCommitted),
        "checkpoint should write status against the original deploy id"
    );
    let releases = store
        .list_service_releases(&manifest.namespace)
        .await
        .expect("list releases");
    assert!(
        releases.iter().any(|release| release.service == "db"),
        "checkpoint commit should publish db release"
    );
    assert!(
        releases.iter().any(|release| release.service == "web"),
        "final commit should publish web release"
    );
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &result.deploy_id,
        "db",
        "succeeded",
        None,
    )
    .await;
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &result.deploy_id,
        "web",
        "succeeded",
        None,
    )
    .await;
    let db_phase = store
        .get_deploy_phase(
            &manifest.namespace,
            &result.deploy_id,
            &DeployPhaseId("db".into()),
        )
        .await
        .expect("get db phase")
        .expect("db phase");
    assert_eq!(
        db_phase.commit_deploy_id,
        Some(DeployId(format!("{}:phase:db", result.deploy_id.0)))
    );
    let web_phase = store
        .get_deploy_phase(
            &manifest.namespace,
            &result.deploy_id,
            &DeployPhaseId("web".into()),
        )
        .await
        .expect("get web phase")
        .expect("web phase");
    assert_eq!(web_phase.commit_deploy_id, Some(result.deploy_id));
}

#[tokio::test]
async fn apply_failure_after_checkpoint_preserves_committed_phase_facts() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = checkpointed_db_web_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController {
        fail_start_service: Some("web".into()),
        ..Default::default()
    });

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("web startup should fail after db checkpoint");

    assert!(
        error
            .to_string()
            .contains("injected start failure for 'web'")
    );
    assert_eq!(backend.commit_count(), 1);
    let last_update = backend
        .last_deploy_status_write()
        .await
        .expect("failed deploy record should be written");
    assert_eq!(last_update.state, DeployState::FailedAfterCheckpoint);
    let releases = store
        .list_service_releases(&manifest.namespace)
        .await
        .expect("list releases");
    assert!(
        releases.iter().any(|release| release.service == "db"),
        "checkpointed db release must remain committed"
    );
    assert!(
        releases.iter().all(|release| release.service != "web"),
        "failed final phase must not publish web release"
    );
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "db",
        "succeeded",
        None,
    )
    .await;
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "web",
        "failed",
        Some("injected start failure for 'web'"),
    )
    .await;
    let db_phase = store
        .get_deploy_phase(
            &manifest.namespace,
            &last_update.deploy_id,
            &DeployPhaseId("db".into()),
        )
        .await
        .expect("get db phase")
        .expect("db phase");
    assert_eq!(
        db_phase.commit_deploy_id,
        Some(DeployId(format!("{}:phase:db", last_update.deploy_id.0)))
    );
    let web_phase = store
        .get_deploy_phase(
            &manifest.namespace,
            &last_update.deploy_id,
            &DeployPhaseId("web".into()),
        )
        .await
        .expect("get web phase")
        .expect("web phase");
    assert_eq!(web_phase.commit_deploy_id, None);
}

#[tokio::test]
async fn apply_failure_after_end_of_deploy_phase_fails_pending_phase() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = end_of_deploy_db_web_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController {
        fail_start_service: Some("web".into()),
        ..Default::default()
    });

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("web startup should fail after db phase work");

    assert!(
        error
            .to_string()
            .contains("injected start failure for 'web'")
    );
    assert_eq!(backend.commit_count(), 0);
    let last_update = backend
        .last_deploy_status_write()
        .await
        .expect("failed deploy record should be written");
    assert_eq!(last_update.state, DeployState::Failed);
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "db",
        "failed",
        Some("injected start failure for 'web'"),
    )
    .await;
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "web",
        "failed",
        Some("injected start failure for 'web'"),
    )
    .await;
}

#[tokio::test]
async fn apply_failure_after_end_of_deploy_phase_fails_prior_pending_phases() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = test_manifest(vec![
        test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17"),
        test_service_spec("web", Placement::Replicated { count: 1 }, "nginx:1.27"),
    ]);
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: Vec::new(),
        phases: vec![
            DeployPhaseIntent {
                phase_id: "db".into(),
                name: Some("Database".into()),
                after: Vec::new(),
                services: vec!["db".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
            DeployPhaseIntent {
                phase_id: "web".into(),
                name: Some("Web".into()),
                after: vec!["db".into()],
                services: vec!["web".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
        ],
    });
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController {
        fail_start_service: Some("web".into()),
        ..Default::default()
    });

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("web startup should fail after db phase work");

    assert!(
        error
            .to_string()
            .contains("injected start failure for 'web'")
    );
    assert_eq!(backend.commit_count(), 0);
    let last_update = backend
        .last_deploy_status_write()
        .await
        .expect("failed deploy record should be written");
    assert_eq!(last_update.state, DeployState::Failed);
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "db",
        "failed",
        Some("injected start failure for 'web'"),
    )
    .await;
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "web",
        "failed",
        Some("injected start failure for 'web'"),
    )
    .await;
}

#[tokio::test]
async fn apply_failure_marks_unstarted_later_phases_failed() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let mut manifest = test_manifest(vec![
        test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17"),
        test_service_spec("web", Placement::Replicated { count: 1 }, "nginx:1.27"),
        test_service_spec("worker", Placement::Replicated { count: 1 }, "busybox:1.36"),
    ]);
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: Vec::new(),
        phases: vec![
            DeployPhaseIntent {
                phase_id: "db".into(),
                name: Some("Database".into()),
                after: Vec::new(),
                services: vec!["db".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
            DeployPhaseIntent {
                phase_id: "web".into(),
                name: Some("Web".into()),
                after: vec!["db".into()],
                services: vec!["web".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
            DeployPhaseIntent {
                phase_id: "worker".into(),
                name: Some("Worker".into()),
                after: vec!["web".into()],
                services: vec!["worker".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
        ],
    });
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController {
        fail_start_service: Some("db".into()),
        ..Default::default()
    });

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("db startup should abort deploy");

    assert!(
        error
            .to_string()
            .contains("injected start failure for 'db'")
    );
    assert_eq!(backend.commit_count(), 0);
    let last_update = backend
        .last_deploy_status_write()
        .await
        .expect("failed deploy record should be written");
    assert_eq!(last_update.state, DeployState::Failed);
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "db",
        "failed",
        Some("injected start failure for 'db'"),
    )
    .await;
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "web",
        "failed",
        Some("injected start failure for 'db'"),
    )
    .await;
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "worker",
        "failed",
        Some("injected start failure for 'db'"),
    )
    .await;
}

#[tokio::test]
async fn apply_running_phase_record_failure_marks_current_and_unstarted_phases_failed() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = end_of_deploy_db_web_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    backend.fail_running_phase_upsert(true);
    let factory = FakeParticipantClient::new(FakeController::default());

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("running phase record failure should abort deploy");

    assert!(error.to_string().contains("injected running phase failure"));
    assert_eq!(backend.commit_count(), 0);
    let last_update = backend
        .last_deploy_status_write()
        .await
        .expect("failed deploy record should be written");
    assert_eq!(last_update.state, DeployState::Failed);
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "db",
        "failed",
        Some("injected running phase failure"),
    )
    .await;
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "web",
        "failed",
        Some("injected running phase failure"),
    )
    .await;
}

#[tokio::test]
async fn apply_pending_phase_seed_failure_marks_already_written_phases_failed() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = end_of_deploy_db_web_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    backend.fail_pending_phase_upsert_on(2);
    let factory = FakeParticipantClient::new(FakeController::default());

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("pending phase seed failure should abort deploy");

    assert!(error.to_string().contains("injected pending phase failure"));
    assert_eq!(backend.commit_count(), 0);
    let last_update = backend
        .last_deploy_status_write()
        .await
        .expect("failed deploy record should be written");
    assert_eq!(last_update.state, DeployState::Failed);
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "db",
        "failed",
        Some("injected pending phase failure"),
    )
    .await;
    assert!(
        store
            .get_deploy_phase(
                &manifest.namespace,
                &last_update.deploy_id,
                &DeployPhaseId("web".into())
            )
            .await
            .expect("get web phase")
            .is_none(),
        "phase whose initial pending write failed should not have a phase record"
    );
}

#[tokio::test]
async fn apply_checkpoint_commit_failure_fails_prior_end_of_deploy_phase() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = end_of_deploy_db_checkpoint_web_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController::default());
    backend.fail_commit(true);

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("checkpoint commit failure should fail deploy");

    assert!(error.to_string().contains("injected commit failure"));
    assert_eq!(backend.commit_count(), 1);
    let last_update = backend
        .last_deploy_status_write()
        .await
        .expect("failed deploy record should be written");
    assert_eq!(last_update.state, DeployState::Failed);
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "db",
        "failed",
        Some("injected commit failure"),
    )
    .await;
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "web",
        "failed",
        Some("injected commit failure"),
    )
    .await;
}

#[tokio::test]
async fn apply_checkpointed_service_commits_changed_mounted_volume() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = checkpointed_db_volume_web_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let db_phase = initial_plan
        .fingerprint()
        .phases
        .into_iter()
        .find(|phase| phase.phase_id == DeployPhaseId("db".into()))
        .expect("db phase");
    assert!(
        db_phase
            .work
            .iter()
            .any(|work| matches!(work, DeployPhaseWork::Volume { volume, .. } if volume == "data")),
        "changed mounted volume should be owned by the checkpointed service phase"
    );
    let factory = FakeParticipantClient::new(FakeController {
        fail_start_service: Some("web".into()),
        ..Default::default()
    });

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("web startup should fail after db checkpoint");

    assert!(
        error
            .to_string()
            .contains("injected start failure for 'web'")
    );
    assert_eq!(backend.commit_count(), 1);
    let volume = store
        .get_volume(&manifest.namespace, "data")
        .await
        .expect("get volume")
        .expect("checkpointed volume record");
    assert_eq!(volume.attached_services, vec!["db"]);
    let releases = store
        .list_service_releases(&manifest.namespace)
        .await
        .expect("list releases");
    assert!(
        releases.iter().any(|release| release.service == "db"),
        "checkpointed db release must be committed with its volume"
    );
}

#[tokio::test]
async fn apply_with_initial_plan_sets_cleanup_pending_after_cleanup_failure() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.27",
    )]);
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    let old_instance = test_instance_status(
        &manifest.namespace,
        "api",
        "slot-0001",
        "machine-a",
        "old-instance",
        &revision_hash,
    );
    store
        .record_instance_status(&old_instance)
        .await
        .expect("seed old instance");
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController {
        fail_remove_instance: Some("old-instance".into()),
        ..Default::default()
    });

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("apply");

    assert_eq!(result.state, crate::model::DeployState::CleanupPending);
    assert_eq!(backend.commit_count(), 1);
    // deploying -> committed point-of-no-return -> post-cert warning refresh -> cleanup_pending
    assert_eq!(backend.deploy_status_write_count(), 4);
    let commit_index = result
        .events
        .iter()
        .position(|event| event.step == "commit")
        .expect("commit event");
    let cleanup_pending_index = result
        .events
        .iter()
        .position(|event| event.step == "cleanup_pending")
        .expect("cleanup pending event");
    assert!(commit_index < cleanup_pending_index);
    assert!(
        result
            .events
            .iter()
            .filter(|event| event.step == "commit")
            .count()
            == 1
    );
    assert_eq!(factory.controller.drain_count(), 1);
    assert_eq!(factory.controller.remove_count(), 1);
}

#[tokio::test]
async fn apply_with_initial_plan_commits_once_after_all_starts_finish() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![
        test_service_spec("api", Placement::Replicated { count: 2 }, "nginx:1.27"),
        test_service_spec("worker", Placement::Replicated { count: 2 }, "busybox:1.0"),
    ]);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController {
        start_delay: Duration::from_millis(10),
        ..Default::default()
    });

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("apply");

    assert_eq!(result.state, crate::model::DeployState::Committed);
    assert_eq!(backend.commit_count(), 1);
    let commit_index = result
        .events
        .iter()
        .position(|event| event.step == "commit")
        .expect("commit event");
    let last_start_index = result
        .events
        .iter()
        .rposition(|event| event.step == "start_candidate")
        .expect("start events");
    assert!(last_start_index < commit_index);
    assert!(
        result
            .events
            .iter()
            .enumerate()
            .skip(commit_index + 1)
            .all(|(_, event)| event.step != "start_candidate")
    );
    assert_default_phase_record(
        &store,
        &manifest.namespace,
        &result.deploy_id,
        "succeeded",
        None,
    )
    .await;
}

#[tokio::test]
async fn prepared_deploy_builds_applying_record_and_revisions() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.27",
    )]);
    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let prepared = PreparedDeploy::new(
        DeployId("deploy-1".into()),
        10,
        local_machine_id.clone(),
        plan,
    )
    .expect("prepared deploy");

    assert_eq!(
        prepared.applying_record().state,
        crate::model::DeployState::Applying
    );
    assert_eq!(prepared.applying_record().started_at, 10);
    assert_eq!(prepared.applying_record().committed_at, None);
    assert_eq!(prepared.revisions().len(), 1);
    let [revision] = prepared.revisions() else {
        panic!("expected one revision");
    };
    assert_eq!(revision.service, "api");
    assert_eq!(revision.created_by, local_machine_id);
}

#[tokio::test]
async fn started_candidates_rejects_missing_started_create_slot() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.27",
    )]);
    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");
    let prepared = PreparedDeploy::new(DeployId("deploy-1".into()), 10, local_machine_id, plan)
        .expect("prepared deploy");

    let error = prepared
        .into_started(HashMap::new())
        .into_commit_plan(Vec::new(), Vec::new())
        .expect_err("missing started candidate should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::MissingStartedInstance {
            service: "api".into(),
            slot: "slot-0001".into()
        })
    );
}

#[tokio::test]
async fn commit_plan_contains_removed_services() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "worker",
        Placement::Replicated { count: 1 },
        "busybox:1.0",
    )]);
    let revision_hash = "old-rev";
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            revision_hash,
            vec![test_slot("slot-0001", "machine-a", "inst-1", revision_hash)],
        ))
        .await
        .expect("seed release");
    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");
    let worker_slot = plan
        .services()
        .iter()
        .find(|service| service.service == "worker")
        .and_then(|service| service.slots.first().map(|slot| slot.slot_id.clone()))
        .expect("worker slot");
    let worker_revision_hash = manifest.services[0].revision_hash().expect("revision hash");
    let prepared = PreparedDeploy::new(DeployId("deploy-1".into()), 10, local_machine_id, plan)
        .expect("prepared deploy");
    let started = HashMap::from([(
        (String::from("worker"), worker_slot.0.clone()),
        test_instance_status(
            &manifest.namespace,
            "worker",
            &worker_slot.0,
            "machine-a",
            "worker-inst-1",
            &worker_revision_hash,
        ),
    )]);

    let commit_plan = prepared
        .into_started(started)
        .into_commit_plan(Vec::new(), Vec::new())
        .expect("commit plan");

    assert_eq!(commit_plan.commit().removed_services, vec!["api"]);
    assert_eq!(commit_plan.commit().releases.len(), 1);
    assert_eq!(
        commit_plan.commit().deploy.state,
        crate::model::DeployState::Committed
    );
    assert!(commit_plan.commit().deploy.committed_at.is_some());
    assert_eq!(
        commit_plan.commit().deploy.committed_at,
        commit_plan.commit().deploy.finished_at
    );
}

#[tokio::test]
async fn commit_plan_contains_branch_lineage() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let source_namespace = Namespace("prod".into());
    let source_spec = test_service_spec("web", Placement::Replicated { count: 1 }, "nginx:1.27");
    let source_revision_hash = source_spec.revision_hash().expect("source revision hash");
    seed_committed_service_release(&store, &source_namespace, source_spec).await;

    let mut manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::Replicated { count: 1 },
        "example/web:pr-39",
    )]);
    manifest.namespace = Namespace("pr-39".into());
    manifest.intent = Some(DeployIntent {
        services: vec![ServiceIntentHint {
            service: "web".into(),
            intent: ServiceIntent::Branch {
                source_namespace: source_namespace.clone(),
                source_service: "web".into(),
            },
        }],
        volumes: Vec::new(),
        phases: Vec::new(),
    });
    let target_revision_hash = manifest.services[0]
        .revision_hash()
        .expect("target revision");
    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve branch plan");
    let slot = plan
        .services()
        .iter()
        .find(|service| service.service == "web")
        .and_then(|service| service.slots.first().map(|slot| slot.slot_id.clone()))
        .expect("web slot");
    let prepared =
        PreparedDeploy::new(DeployId("deploy-branch".into()), 10, local_machine_id, plan)
            .expect("prepared deploy");
    let started = HashMap::from([(
        (String::from("web"), slot.0.clone()),
        test_instance_status(
            &manifest.namespace,
            "web",
            &slot.0,
            "machine-a",
            "web-inst-1",
            &target_revision_hash,
        ),
    )]);

    let commit_plan = prepared
        .into_started(started)
        .into_commit_plan(Vec::new(), Vec::new())
        .expect("commit plan");

    assert_eq!(commit_plan.commit().branch_lineage.len(), 1);
    let lineage = &commit_plan.commit().branch_lineage[0];
    assert_eq!(lineage.namespace, Namespace("pr-39".into()));
    assert_eq!(lineage.service, "web");
    assert_eq!(lineage.revision_hash, target_revision_hash);
    assert_eq!(lineage.source_namespace, source_namespace);
    assert_eq!(lineage.source_service, "web");
    assert_eq!(lineage.source_revision_hash, source_revision_hash);
    assert_eq!(lineage.deploy_id, DeployId("deploy-branch".into()));
}

#[derive(Clone, Default)]
struct FakeController {
    open_delay: Duration,
    start_delay: Duration,
    fail_open_machine: Option<String>,
    fail_start_service: Option<String>,
    fail_start_after_create_service: Option<String>,
    fail_remove_instance: Option<String>,
    fail_move_volume: Option<String>,
    fail_clone_volume: Option<String>,
    fail_cleanup_clone_volume: Option<String>,
    open_active: Arc<AtomicUsize>,
    max_open: Arc<AtomicUsize>,
    move_count: Arc<AtomicUsize>,
    clone_count: Arc<AtomicUsize>,
    clone_cleanup_count: Arc<AtomicUsize>,
    start_count: Arc<AtomicUsize>,
    start_active: Arc<AtomicUsize>,
    max_global_start: Arc<AtomicUsize>,
    drain_count: Arc<AtomicUsize>,
    remove_count: Arc<AtomicUsize>,
    machine_state: Arc<std::sync::Mutex<HashMap<String, usize>>>,
    machine_max: Arc<std::sync::Mutex<HashMap<String, usize>>>,
    start_log_entries: Arc<Mutex<Vec<String>>>,
    operation_log_entries: Arc<Mutex<Vec<String>>>,
    start_requests: Arc<Mutex<Vec<StartCandidateRequest>>>,
    move_requests: Arc<Mutex<Vec<MoveVolumeRequest>>>,
    clone_requests: Arc<Mutex<Vec<CloneVolumeRequest>>>,
    clone_cleanup_requests: Arc<Mutex<Vec<String>>>,
    inspect_instances: Arc<Mutex<Vec<InstanceStatusRecord>>>,
}

impl FakeController {
    fn max_open_seen(&self) -> usize {
        self.max_open.load(Ordering::SeqCst)
    }

    fn start_count(&self) -> usize {
        self.start_count.load(Ordering::SeqCst)
    }

    fn move_count(&self) -> usize {
        self.move_count.load(Ordering::SeqCst)
    }

    fn clone_count(&self) -> usize {
        self.clone_count.load(Ordering::SeqCst)
    }

    fn clone_cleanup_count(&self) -> usize {
        self.clone_cleanup_count.load(Ordering::SeqCst)
    }

    fn max_global_start_seen(&self) -> usize {
        self.max_global_start.load(Ordering::SeqCst)
    }

    fn drain_count(&self) -> usize {
        self.drain_count.load(Ordering::SeqCst)
    }

    fn remove_count(&self) -> usize {
        self.remove_count.load(Ordering::SeqCst)
    }

    fn max_machine_start_seen(&self, machine_id: &str) -> usize {
        self.machine_max
            .lock()
            .expect("machine max lock")
            .get(machine_id)
            .copied()
            .unwrap_or_default()
    }

    async fn start_log(&self) -> Vec<String> {
        self.start_log_entries.lock().await.clone()
    }

    async fn operation_log(&self) -> Vec<String> {
        self.operation_log_entries.lock().await.clone()
    }

    async fn start_requests(&self) -> Vec<StartCandidateRequest> {
        self.start_requests.lock().await.clone()
    }

    async fn move_requests(&self) -> Vec<MoveVolumeRequest> {
        self.move_requests.lock().await.clone()
    }

    async fn clone_requests(&self) -> Vec<CloneVolumeRequest> {
        self.clone_requests.lock().await.clone()
    }

    async fn set_inspect_instances(&self, instances: Vec<InstanceStatusRecord>) {
        *self.inspect_instances.lock().await = instances;
    }

    async fn on_open_start(&self) {
        let current = self.open_active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_open.fetch_max(current, Ordering::SeqCst);
        sleep(self.open_delay).await;
        self.open_active.fetch_sub(1, Ordering::SeqCst);
    }

    fn should_fail_open(&self, machine_id: &MachineId) -> bool {
        self.fail_open_machine.as_deref() == Some(machine_id.0.as_str())
    }

    async fn on_start_begin(&self, machine_id: &MachineId, service: &str, slot_id: &SlotId) {
        self.start_count.fetch_add(1, Ordering::SeqCst);
        let global = self.start_active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_global_start.fetch_max(global, Ordering::SeqCst);
        {
            let mut machine_state = self.machine_state.lock().expect("machine state lock");
            let current = machine_state.entry(machine_id.0.clone()).or_default();
            *current += 1;
            let mut machine_max = self.machine_max.lock().expect("machine max lock");
            let max = machine_max.entry(machine_id.0.clone()).or_default();
            *max = (*max).max(*current);
        }
        self.start_log_entries
            .lock()
            .await
            .push(format!("{service}:{machine_id}:{slot_id}"));
        self.operation_log_entries
            .lock()
            .await
            .push(format!("start:{service}:{machine_id}:{slot_id}"));
    }

    fn should_fail_start(&self, service: &str) -> bool {
        self.fail_start_service.as_deref() == Some(service)
    }

    fn should_fail_start_after_create(&self, service: &str) -> bool {
        self.fail_start_after_create_service.as_deref() == Some(service)
    }

    async fn on_start_end(&self, machine_id: &MachineId) {
        self.start_active.fetch_sub(1, Ordering::SeqCst);
        let mut machine_state = self.machine_state.lock().expect("machine state lock");
        let Some(current) = machine_state.get_mut(&machine_id.0) else {
            return;
        };
        *current -= 1;
    }

    async fn on_drain(&self, instance_id: &InstanceId) {
        self.drain_count.fetch_add(1, Ordering::SeqCst);
        self.operation_log_entries
            .lock()
            .await
            .push(format!("drain:{instance_id}"));
    }

    async fn on_remove(&self, instance_id: &InstanceId) {
        self.remove_count.fetch_add(1, Ordering::SeqCst);
        self.operation_log_entries
            .lock()
            .await
            .push(format!("remove:{instance_id}"));
    }

    fn should_fail_remove(&self, instance_id: &InstanceId) -> bool {
        self.fail_remove_instance.as_deref() == Some(instance_id.0.as_str())
    }

    async fn on_move_volume(&self, machine_id: &MachineId, request: &MoveVolumeRequest) {
        self.move_count.fetch_add(1, Ordering::SeqCst);
        self.move_requests.lock().await.push(request.clone());
        self.operation_log_entries.lock().await.push(format!(
            "move:{}:{}:{}:{}",
            request.volume, machine_id, request.from_machine, request.to_machine
        ));
    }

    fn should_fail_move(&self, volume: &str) -> bool {
        self.fail_move_volume.as_deref() == Some(volume)
    }

    async fn on_clone_volume(&self, machine_id: &MachineId, request: &CloneVolumeRequest) {
        self.clone_count.fetch_add(1, Ordering::SeqCst);
        self.clone_requests.lock().await.push(request.clone());
        self.operation_log_entries.lock().await.push(format!(
            "clone:{}:{}:{}/{}",
            request.volume, machine_id, request.source_namespace, request.source_volume
        ));
    }

    fn should_fail_clone(&self, volume: &str) -> bool {
        self.fail_clone_volume.as_deref() == Some(volume)
    }

    fn should_fail_cleanup_clone(&self, volume: &str) -> bool {
        self.fail_cleanup_clone_volume.as_deref() == Some(volume)
    }

    async fn on_cleanup_uncommitted_volume_clone(&self, volume: &str) {
        self.clone_cleanup_count.fetch_add(1, Ordering::SeqCst);
        self.clone_cleanup_requests
            .lock()
            .await
            .push(volume.to_string());
        self.operation_log_entries
            .lock()
            .await
            .push(format!("cleanup_clone:{volume}"));
    }
}

struct FakeParticipantClient {
    controller: FakeController,
}

impl FakeParticipantClient {
    fn new(controller: FakeController) -> Self {
        Self { controller }
    }
}

#[async_trait::async_trait]
impl DeployParticipantClient for FakeParticipantClient {
    fn supports_volume_moves(&self) -> bool {
        true
    }

    fn supports_volume_clones(&self) -> bool {
        true
    }

    async fn inspect_namespace(
        &self,
        machine: &MachineMembership,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        _coordinator_id: &MachineId,
    ) -> Result<Vec<InstanceStatusRecord>> {
        self.controller.on_open_start().await;
        if self.controller.should_fail_open(&machine.id) {
            return Err(ployz_types::error::Error::operation(
                "fake_open",
                format!("injected open failure for '{}'", machine.id),
            ));
        }
        Ok(self
            .controller
            .inspect_instances
            .lock()
            .await
            .iter()
            .filter(|status| status.machine_id == machine.id)
            .cloned()
            .collect())
    }

    async fn start_candidate(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        req: StartCandidateRequest,
    ) -> Result<InstanceStatusRecord> {
        self.controller
            .start_requests
            .lock()
            .await
            .push(req.clone());
        self.controller
            .on_start_begin(machine_id, &req.service, &req.slot_id)
            .await;
        if self.controller.should_fail_start(&req.service) {
            self.controller.on_start_end(machine_id).await;
            return Err(ployz_types::error::Error::operation(
                "fake_start",
                format!("injected start failure for '{}'", req.service),
            ));
        }
        sleep(self.controller.start_delay).await;
        if self.controller.should_fail_start_after_create(&req.service) {
            self.controller.on_start_end(machine_id).await;
            return Err(ployz_types::error::Error::operation(
                "fake_start_after_create",
                format!("injected post-create start failure for '{}'", req.service),
            ));
        }
        self.controller.on_start_end(machine_id).await;
        Ok(InstanceStatusRecord {
            instance_id: req.instance_id.clone(),
            namespace: namespace.clone(),
            service: req.service,
            slot_id: req.slot_id,
            machine_id: machine_id.clone(),
            revision_hash: "fake-revision".into(),
            deploy_id: deploy_id.clone(),
            docker_container_id: format!("container-{}", req.instance_id.0),
            overlay_ip: None,
            backend_ports: BTreeMap::new(),
            phase: InstancePhase::Ready,
            ready: true,
            drain_state: DrainState::None,
            error: None,
            started_at: 0,
            updated_at: 0,
        })
    }

    async fn move_volume(
        &self,
        machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        request: MoveVolumeRequest,
    ) -> Result<MoveVolumeResult> {
        self.controller.on_move_volume(machine_id, &request).await;
        if self.controller.should_fail_move(&request.volume) {
            return Err(ployz_types::error::Error::operation(
                "fake_move_volume",
                format!("injected move failure for '{}'", request.volume),
            ));
        }
        Ok(MoveVolumeResult {
            snapshot: request.snapshot,
            snapshot_guid: 42,
            bytes_transferred: 4096,
        })
    }

    async fn clone_volume(
        &self,
        machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        request: CloneVolumeRequest,
    ) -> Result<CloneVolumeResult> {
        self.controller.on_clone_volume(machine_id, &request).await;
        if self.controller.should_fail_clone(&request.volume) {
            return Err(ployz_types::error::Error::operation(
                "fake_clone_volume",
                format!("injected clone failure for '{}'", request.volume),
            ));
        }
        Ok(CloneVolumeResult {
            snapshot: request.snapshot,
            snapshot_guid: 84,
            target_dataset: format!("tank/ployz/{}", request.volume),
        })
    }

    async fn cleanup_volume_clone(
        &self,
        _machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        request: CleanupVolumeCloneRequest,
    ) -> Result<()> {
        let volume = request.volume.as_str();
        self.controller
            .on_cleanup_uncommitted_volume_clone(volume)
            .await;
        if self.controller.should_fail_cleanup_clone(volume) {
            return Err(ployz_types::error::Error::operation(
                "fake_cleanup_volume_clone",
                format!("injected cleanup failure for '{volume}'"),
            ));
        }
        Ok(())
    }

    async fn drain_instance(
        &self,
        _machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        instance_id: &InstanceId,
    ) -> Result<()> {
        self.controller.on_drain(instance_id).await;
        Ok(())
    }

    async fn remove_instance(
        &self,
        _machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        instance_id: &InstanceId,
    ) -> Result<()> {
        self.controller.on_remove(instance_id).await;
        if self.controller.should_fail_remove(instance_id) {
            return Err(ployz_types::error::Error::operation(
                "fake_remove",
                format!("injected remove failure for '{}'", instance_id),
            ));
        }
        Ok(())
    }
}

#[derive(Default)]
struct UnsupportedParticipantClient {
    inspect_count: AtomicUsize,
}

impl UnsupportedParticipantClient {
    fn inspect_count(&self) -> usize {
        self.inspect_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl DeployParticipantClient for UnsupportedParticipantClient {
    async fn inspect_namespace(
        &self,
        _machine: &MachineMembership,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        _coordinator_id: &MachineId,
    ) -> Result<Vec<InstanceStatusRecord>> {
        self.inspect_count.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }

    async fn start_candidate(
        &self,
        _machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        _request: StartCandidateRequest,
    ) -> Result<InstanceStatusRecord> {
        Err(Error::operation("unsupported_participant", "start"))
    }

    async fn drain_instance(
        &self,
        _machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        _instance_id: &InstanceId,
    ) -> Result<()> {
        Err(Error::operation("unsupported_participant", "drain"))
    }

    async fn remove_instance(
        &self,
        _machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        _instance_id: &InstanceId,
    ) -> Result<()> {
        Err(Error::operation("unsupported_participant", "remove"))
    }
}

struct FailingParticipantProbe {
    machine_id: MachineId,
}

#[async_trait]
impl ParticipantProbe for FailingParticipantProbe {
    async fn ping(&self, machine: &MachineMembership) -> std::result::Result<(), ProbeError> {
        if machine.id == self.machine_id {
            return Err(ProbeError {
                kind: ProbeErrorKind::Timeout,
                detail: "injected probe timeout".into(),
            });
        }
        Ok(())
    }
}

async fn seeded_store_with_machines(machine_ids: &[&str]) -> StoreDriver {
    let store = StoreDriver::memory();
    for machine_id in machine_ids {
        store
            .upsert_self_machine(&test_machine(machine_id, MachineLifecycle::Active))
            .await
            .expect("seed machine");
    }
    store
}

async fn counting_store_with_machines(machine_ids: &[&str]) -> (StoreDriver, Arc<CountingBackend>) {
    let backend = Arc::new(CountingBackend::new());
    let store = StoreDriver::new(
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
    );
    for machine_id in machine_ids {
        store
            .upsert_self_machine(&test_machine(machine_id, MachineLifecycle::Active))
            .await
            .expect("seed machine");
    }
    (store, backend)
}

async fn assert_default_phase_record(
    store: &StoreDriver,
    namespace: &Namespace,
    deploy_id: &DeployId,
    expected_state: &str,
    failure_contains: Option<&str>,
) {
    let phases = store
        .list_deploy_phases(namespace, deploy_id)
        .await
        .expect("list deploy phases");
    let [phase] = phases.as_slice() else {
        panic!("expected one default deploy phase record, got {phases:?}");
    };
    assert_eq!(phase.namespace, *namespace);
    assert_eq!(phase.deploy_id, *deploy_id);
    assert_eq!(phase.phase_id, DeployPhaseId("deploy".into()));
    assert_eq!(phase.name, "Deploy");
    assert_eq!(phase.order, 0);
    assert_eq!(phase.commit_policy, DeployPhaseCommitPolicy::EndOfDeploy);
    assert_eq!(phase.rollback_policy, DeployPhaseRollbackPolicy::Reversible);
    match (&phase.state, expected_state) {
        (DeployPhaseState::Running, "running") => {}
        (DeployPhaseState::Succeeded { completed_at }, "succeeded") => {
            assert!(*completed_at >= phase.started_at);
        }
        (
            DeployPhaseState::Failed {
                completed_at,
                failure,
            },
            "failed",
        ) => {
            assert!(*completed_at >= phase.started_at);
            let Some(expected) = failure_contains else {
                panic!("failed phase assertion requires expected failure text");
            };
            assert!(!failure.code.is_empty());
            assert!(
                failure.message.contains(expected),
                "failure message should contain {expected:?}: {}",
                failure.message
            );
        }
        (actual, expected) => panic!("expected phase state {expected}, got {actual:?}"),
    }
}

async fn assert_phase_record_state(
    store: &StoreDriver,
    namespace: &Namespace,
    deploy_id: &DeployId,
    phase_id: &str,
    expected_state: &str,
    failure_contains: Option<&str>,
) {
    let phase = store
        .get_deploy_phase(namespace, deploy_id, &DeployPhaseId(phase_id.into()))
        .await
        .expect("get deploy phase")
        .unwrap_or_else(|| panic!("expected deploy phase {phase_id}"));
    match (&phase.state, expected_state) {
        (DeployPhaseState::Running, "running") => {}
        (DeployPhaseState::Succeeded { completed_at }, "succeeded") => {
            assert!(*completed_at >= phase.started_at);
        }
        (
            DeployPhaseState::Failed {
                completed_at,
                failure,
            },
            "failed",
        ) => {
            assert!(*completed_at >= phase.started_at);
            let Some(expected) = failure_contains else {
                panic!("failed phase assertion requires expected failure text");
            };
            assert!(
                failure.message.contains(expected),
                "failure message should contain {expected:?}: {}",
                failure.message
            );
        }
        (actual, expected) => panic!("expected phase state {expected}, got {actual:?}"),
    }
}

fn test_manifest(services: Vec<ServiceSpec>) -> DeployManifest {
    DeployManifest {
        namespace: Namespace("test".into()),
        intent: None,
        volumes: Vec::new(),
        services,
    }
}

fn checkpointed_db_web_manifest() -> DeployManifest {
    let mut manifest = test_manifest(vec![
        test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17"),
        test_service_spec("web", Placement::Replicated { count: 1 }, "nginx:1.27"),
    ]);
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: Vec::new(),
        phases: vec![
            DeployPhaseIntent {
                phase_id: "db".into(),
                name: Some("Database".into()),
                after: Vec::new(),
                services: vec!["db".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::Checkpoint,
                rollback_policy: DeployPhaseRollbackPolicy::ForwardOnly,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
            DeployPhaseIntent {
                phase_id: "web".into(),
                name: Some("Web".into()),
                after: vec!["db".into()],
                services: vec!["web".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
        ],
    });
    manifest
}

fn end_of_deploy_db_web_manifest() -> DeployManifest {
    let mut manifest = test_manifest(vec![
        test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17"),
        test_service_spec("web", Placement::Replicated { count: 1 }, "nginx:1.27"),
    ]);
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: Vec::new(),
        phases: vec![
            DeployPhaseIntent {
                phase_id: "db".into(),
                name: Some("Database".into()),
                after: Vec::new(),
                services: vec!["db".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
            DeployPhaseIntent {
                phase_id: "web".into(),
                name: Some("Web".into()),
                after: vec!["db".into()],
                services: vec!["web".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
        ],
    });
    manifest
}

fn end_of_deploy_db_checkpoint_web_manifest() -> DeployManifest {
    let mut manifest = end_of_deploy_db_web_manifest();
    let Some(intent) = manifest.intent.as_mut() else {
        panic!("expected deploy intent");
    };
    let Some(web_phase) = intent
        .phases
        .iter_mut()
        .find(|phase| phase.phase_id == "web")
    else {
        panic!("expected web phase");
    };
    web_phase.commit_policy = DeployPhaseCommitPolicy::Checkpoint;
    web_phase.rollback_policy = DeployPhaseRollbackPolicy::ForwardOnly;
    manifest
}

fn checkpointed_db_volume_web_manifest() -> DeployManifest {
    let mut db = test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17");
    db.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![
        db,
        test_service_spec("web", Placement::Replicated { count: 1 }, "nginx:1.27"),
    ]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: Vec::new(),
        phases: vec![
            DeployPhaseIntent {
                phase_id: "db".into(),
                name: Some("Database".into()),
                after: Vec::new(),
                services: vec!["db".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::Checkpoint,
                rollback_policy: DeployPhaseRollbackPolicy::ForwardOnly,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
            DeployPhaseIntent {
                phase_id: "web".into(),
                name: Some("Web".into()),
                after: vec!["db".into()],
                services: vec!["web".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
        ],
    });
    manifest
}

fn volume_manifest() -> DeployManifest {
    let mut service = test_service_spec("db", Placement::Replicated { count: 1 }, "postgres:17");
    service.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![service]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest
}

fn test_volume(name: &str, scope: VolumeScope) -> VolumeDeclaration {
    VolumeDeclaration {
        name: name.into(),
        scope,
        quota: "1G".into(),
        mode: "0750".into(),
        owner: "999:999".into(),
    }
}

async fn seed_volume(
    store: &StoreDriver,
    namespace: &Namespace,
    volume_name: &str,
    machine_id: &str,
) {
    seed_volume_with(
        store,
        namespace,
        volume_name,
        machine_id,
        "1G",
        "0750",
        "999:999",
    )
    .await;
}

async fn seed_volume_with(
    store: &StoreDriver,
    namespace: &Namespace,
    volume_name: &str,
    machine_id: &str,
    quota: &str,
    mode: &str,
    owner: &str,
) {
    seed_volume_with_attached_services(
        store,
        namespace,
        volume_name,
        machine_id,
        quota,
        mode,
        owner,
        Vec::new(),
    )
    .await;
}

async fn seed_volume_with_attached_services(
    store: &StoreDriver,
    namespace: &Namespace,
    volume_name: &str,
    machine_id: &str,
    quota: &str,
    mode: &str,
    owner: &str,
    attached_services: Vec<String>,
) {
    seed_volume_with_scope_and_attached_services(
        store,
        namespace,
        volume_name,
        machine_id,
        VolumeScope::Single,
        quota,
        mode,
        owner,
        attached_services,
    )
    .await;
}

async fn seed_volume_with_scope(
    store: &StoreDriver,
    namespace: &Namespace,
    volume_name: &str,
    machine_id: &str,
    scope: VolumeScope,
) {
    seed_volume_with_scope_and_attached_services(
        store,
        namespace,
        volume_name,
        machine_id,
        scope,
        "1G",
        "0750",
        "999:999",
        Vec::new(),
    )
    .await;
}

async fn seed_volume_with_scope_and_attached_services(
    store: &StoreDriver,
    namespace: &Namespace,
    volume_name: &str,
    machine_id: &str,
    scope: VolumeScope,
    quota: &str,
    mode: &str,
    owner: &str,
    attached_services: Vec<String>,
) {
    let deploy_id = DeployId(format!("seed-{volume_name}"));
    let volume = VolumeRecord {
        namespace: namespace.clone(),
        volume_name: volume_name.into(),
        scope,
        machine_id: MachineId(machine_id.into()),
        quota: quota.into(),
        mode: mode.into(),
        owner: owner.into(),
        attached_services,
        created_at: 1,
        created_by_deploy_id: deploy_id.clone(),
        last_modified_at: 1,
        last_modified_by_deploy_id: deploy_id.clone(),
    };
    let deploy = DeployRecord {
        deploy_id,
        namespace: namespace.clone(),
        coordinator_machine_id: MachineId("local".into()),
        manifest_hash: "seed".into(),
        state: DeployState::Committed,
        started_at: 1,
        committed_at: Some(1),
        finished_at: Some(1),
        summary_json: "{}".into(),
    };
    store
        .commit_deploy(&DeployCommit {
            namespace: namespace.clone(),
            revisions: Vec::new(),
            removed_services: Vec::new(),
            removed_volumes: Vec::new(),
            branch_lineage: Vec::new(),
            volume_movements: Vec::new(),
            volume_branches: Vec::new(),
            phase_commits: Vec::new(),
            releases: Vec::new(),
            volumes: vec![volume],
            deploy,
        })
        .await
        .expect("seed volume");
}

fn test_service_spec(name: &str, placement: Placement, image: &str) -> ServiceSpec {
    ServiceSpec {
        name: name.into(),
        placement,
        template: ContainerSpec {
            image: image.into(),
            command: None,
            entrypoint: None,
            env: BTreeMap::new(),
            mounts: Vec::new(),
            cap_add: Vec::new(),
            cap_drop: Vec::new(),
            privileged: false,
            user: None,
            stop_grace_period: None,
            pid_mode: None,
            pull_policy: PullPolicy::IfNotPresent,
            resources: Resources::empty(),
            sysctls: BTreeMap::new(),
        },
        network: NetworkMode::Overlay,
        service_ports: Vec::new(),
        publish: Vec::new(),
        routes: Vec::new(),
        readiness: None,
        rollout: RolloutStrategy::Recreate,
        labels: BTreeMap::new(),
        restart: RestartPolicy::UnlessStopped,
    }
}

fn http_route_service_spec(name: &str, hostname: &str) -> ServiceSpec {
    let mut spec = test_service_spec(name, Placement::Replicated { count: 1 }, "nginx:1.27");
    spec.service_ports = vec![ServicePort {
        name: "http".into(),
        container_port: 8080,
        protocol: PortProtocol::Tcp,
    }];
    spec.routes = vec![RouteSpec::Http(HttpRoute {
        service_port: "http".into(),
        hostnames: vec![hostname.into()],
        path_prefix: "/".into(),
    })];
    spec
}

async fn seed_committed_http_release(
    store: &StoreDriver,
    namespace: &str,
    service: &str,
    hostname: &str,
) {
    let namespace = Namespace(namespace.into());
    let spec = http_route_service_spec(service, hostname);
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .commit_deploy(&DeployCommit {
            namespace: namespace.clone(),
            revisions: vec![crate::model::ServiceRevisionRecord {
                namespace: namespace.clone(),
                service: service.into(),
                revision_hash: revision_hash.clone(),
                spec_json: spec
                    .canonical_revision_json()
                    .expect("canonical revision json"),
                created_by: MachineId("seed".into()),
                created_at: 0,
            }],
            removed_services: Vec::new(),
            removed_volumes: Vec::new(),
            branch_lineage: Vec::new(),
            volume_movements: Vec::new(),
            volume_branches: Vec::new(),
            phase_commits: Vec::new(),
            releases: vec![test_release(
                &namespace,
                service,
                &revision_hash,
                Vec::new(),
            )],
            volumes: Vec::new(),
            deploy: test_deploy_record(&namespace, "seed-deploy"),
        })
        .await
        .expect("seed release");
}

async fn seed_committed_service_release(
    store: &StoreDriver,
    namespace: &Namespace,
    spec: ServiceSpec,
) {
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .commit_deploy(&DeployCommit {
            namespace: namespace.clone(),
            revisions: vec![crate::model::ServiceRevisionRecord {
                namespace: namespace.clone(),
                service: spec.name.clone(),
                revision_hash: revision_hash.clone(),
                spec_json: spec
                    .canonical_revision_json()
                    .expect("canonical revision json"),
                created_by: MachineId("seed".into()),
                created_at: 0,
            }],
            removed_services: Vec::new(),
            removed_volumes: Vec::new(),
            branch_lineage: Vec::new(),
            volume_movements: Vec::new(),
            volume_branches: Vec::new(),
            phase_commits: Vec::new(),
            releases: vec![test_release(
                namespace,
                &spec.name,
                &revision_hash,
                Vec::new(),
            )],
            volumes: Vec::new(),
            deploy: test_deploy_record(namespace, "seed-deploy"),
        })
        .await
        .expect("seed service release");
}

fn test_release(
    namespace: &Namespace,
    service: &str,
    revision_hash: &str,
    slots: Vec<ServiceReleaseSlot>,
) -> ServiceReleaseRecord {
    ServiceReleaseRecord {
        namespace: namespace.clone(),
        service: service.into(),
        release: ServiceRelease {
            primary_revision_hash: revision_hash.into(),
            referenced_revision_hashes: vec![revision_hash.into()],
            routing: ServiceRoutingPolicy::Direct {
                revision_hash: revision_hash.into(),
            },
            slots,
            updated_by_deploy_id: DeployId("deploy-1".into()),
            updated_at: 0,
        },
    }
}

fn test_deploy_record(namespace: &Namespace, deploy_id: &str) -> DeployRecord {
    DeployRecord {
        deploy_id: DeployId(deploy_id.into()),
        namespace: namespace.clone(),
        coordinator_machine_id: MachineId("local".into()),
        manifest_hash: "manifest".into(),
        state: DeployState::Committed,
        started_at: 0,
        committed_at: Some(0),
        finished_at: Some(0),
        summary_json: "{}".into(),
    }
}

fn test_slot(
    slot_id: &str,
    machine_id: &str,
    instance_id: &str,
    revision_hash: &str,
) -> ServiceReleaseSlot {
    ServiceReleaseSlot {
        slot_id: SlotId(slot_id.into()),
        machine_id: MachineId(machine_id.into()),
        active_instance_id: InstanceId(instance_id.into()),
        revision_hash: revision_hash.into(),
    }
}

fn test_machine(id: &str, lifecycle: MachineLifecycle) -> MachineMembership {
    test_machine_in_region(id, lifecycle, RegionRole::HomeData)
}

fn test_machine_in_region(
    id: &str,
    lifecycle: MachineLifecycle,
    region_role: RegionRole,
) -> MachineMembership {
    MachineMembership {
        id: MachineId(id.into()),
        public_key: PublicKey([7; 32]),
        overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
        topology: MachineTopology::local(),
        region_role,
        subnet: None,
        bridge_ip: None,
        endpoints: vec!["127.0.0.1:51820".into()],
        lifecycle,
        storage: true,
        storage_participation: ployz_types::model::StorageParticipation::default_authority(),
        created_at: 0,
        updated_at: 0,
        labels: BTreeMap::new(),
    }
}

fn test_instance_status(
    namespace: &Namespace,
    service: &str,
    slot_id: &str,
    machine_id: &str,
    instance_id: &str,
    revision_hash: &str,
) -> InstanceStatusRecord {
    InstanceStatusRecord {
        instance_id: InstanceId(instance_id.into()),
        namespace: namespace.clone(),
        service: service.into(),
        slot_id: SlotId(slot_id.into()),
        machine_id: MachineId(machine_id.into()),
        revision_hash: revision_hash.into(),
        deploy_id: DeployId("previous-deploy".into()),
        docker_container_id: format!("container-{instance_id}"),
        overlay_ip: None,
        backend_ports: BTreeMap::new(),
        phase: InstancePhase::Ready,
        ready: true,
        drain_state: DrainState::None,
        error: None,
        started_at: 0,
        updated_at: 0,
    }
}

struct CountingBackend {
    store: Arc<MemoryStore>,
    service: Arc<MemoryService>,
    commit_calls: AtomicUsize,
    deploy_status_writes: AtomicUsize,
    committed_status_writes: AtomicUsize,
    fail_commit: AtomicBool,
    fail_committed_status_writes: AtomicBool,
    fail_committed_status_writes_after_first: AtomicBool,
    fail_succeeded_phase_upsert: AtomicBool,
    fail_running_phase_upsert: AtomicBool,
    pending_phase_upserts: AtomicUsize,
    fail_pending_phase_upsert_on: AtomicUsize,
    deploy_status_records: Mutex<Vec<DeployRecord>>,
}

impl CountingBackend {
    fn new() -> Self {
        Self {
            store: Arc::new(MemoryStore::new()),
            service: Arc::new(MemoryService::new()),
            commit_calls: AtomicUsize::new(0),
            deploy_status_writes: AtomicUsize::new(0),
            committed_status_writes: AtomicUsize::new(0),
            fail_commit: AtomicBool::new(false),
            fail_committed_status_writes: AtomicBool::new(false),
            fail_committed_status_writes_after_first: AtomicBool::new(false),
            fail_succeeded_phase_upsert: AtomicBool::new(false),
            fail_running_phase_upsert: AtomicBool::new(false),
            pending_phase_upserts: AtomicUsize::new(0),
            fail_pending_phase_upsert_on: AtomicUsize::new(0),
            deploy_status_records: Mutex::new(Vec::new()),
        }
    }

    fn commit_count(&self) -> usize {
        self.commit_calls.load(Ordering::SeqCst)
    }

    fn deploy_status_write_count(&self) -> usize {
        self.deploy_status_writes.load(Ordering::SeqCst)
    }

    fn reset_counts(&self) {
        self.commit_calls.store(0, Ordering::SeqCst);
        self.deploy_status_writes.store(0, Ordering::SeqCst);
        self.committed_status_writes.store(0, Ordering::SeqCst);
    }

    fn fail_committed_status_writes_after_first(&self, fail: bool) {
        self.fail_committed_status_writes_after_first
            .store(fail, Ordering::SeqCst);
    }

    fn fail_committed_status_writes(&self, fail: bool) {
        self.fail_committed_status_writes
            .store(fail, Ordering::SeqCst);
    }

    fn fail_commit(&self, fail: bool) {
        self.fail_commit.store(fail, Ordering::SeqCst);
    }

    fn fail_succeeded_phase_upsert(&self, fail: bool) {
        self.fail_succeeded_phase_upsert
            .store(fail, Ordering::SeqCst);
    }

    fn fail_running_phase_upsert(&self, fail: bool) {
        self.fail_running_phase_upsert.store(fail, Ordering::SeqCst);
    }

    fn fail_pending_phase_upsert_on(&self, call: usize) {
        self.pending_phase_upserts.store(0, Ordering::SeqCst);
        self.fail_pending_phase_upsert_on
            .store(call, Ordering::SeqCst);
    }

    async fn last_deploy_status_write(&self) -> Option<DeployRecord> {
        self.deploy_status_records.lock().await.last().cloned()
    }

    async fn deploy_status_writes(&self) -> Vec<DeployRecord> {
        self.deploy_status_records.lock().await.clone()
    }
}

#[async_trait]
impl MachineMembershipStore for CountingBackend {
    async fn init(&self) -> PloyzResult<()> {
        self.store.init().await
    }

    async fn list_machines(&self) -> PloyzResult<Vec<MachineMembership>> {
        self.store.list_machines().await
    }

    async fn upsert_self_machine(&self, record: &MachineMembership) -> PloyzResult<()> {
        self.store.upsert_self_machine(record).await
    }

    async fn delete_machine(&self, id: &MachineId) -> PloyzResult<()> {
        self.store.delete_machine(id).await
    }

    async fn subscribe_machines(&self) -> PloyzResult<MachineSubscription> {
        self.store.subscribe_machines().await
    }
}

#[async_trait]
impl InviteStore for CountingBackend {
    async fn create_invite(&self, invite: &ployz_types::model::InviteRecord) -> PloyzResult<()> {
        self.store.create_invite(invite).await
    }

    async fn get_invite(
        &self,
        invite_id: &str,
    ) -> PloyzResult<Option<ployz_types::model::InviteRecord>> {
        self.store.get_invite(invite_id).await
    }

    async fn list_invites(&self) -> PloyzResult<Vec<ployz_types::model::InviteRecord>> {
        self.store.list_invites().await
    }

    async fn redeem_invite(
        &self,
        invite_id: &str,
        machine_id: &MachineId,
        now_unix_secs: u64,
    ) -> PloyzResult<ployz_types::model::InviteRecord> {
        self.store
            .redeem_invite(invite_id, machine_id, now_unix_secs)
            .await
    }

    async fn revoke_invite(
        &self,
        invite_id: &str,
        now_unix_secs: u64,
    ) -> PloyzResult<ployz_types::model::InviteRecord> {
        self.store.revoke_invite(invite_id, now_unix_secs).await
    }
}

#[async_trait]
impl RoutingStateStore for CountingBackend {
    async fn load_routing_state(&self) -> PloyzResult<crate::model::RoutingState> {
        self.store.load_routing_state().await
    }

    async fn subscribe_routing_events(&self) -> PloyzResult<RoutingEventSubscription> {
        RoutingStateStore::subscribe_routing_events(self.store.as_ref()).await
    }
}

#[async_trait]
impl ImageAvailabilityStore for CountingBackend {
    async fn upsert_image_availability(
        &self,
        record: &ployz_types::model::ImageAvailabilityRecord,
    ) -> PloyzResult<()> {
        self.store.upsert_image_availability(record).await
    }

    async fn get_image_availability(
        &self,
        machine_id: &MachineId,
        digest: &ployz_types::model::ImageDigest,
    ) -> PloyzResult<Option<ployz_types::model::ImageAvailabilityRecord>> {
        self.store.get_image_availability(machine_id, digest).await
    }

    async fn list_image_availability(
        &self,
    ) -> PloyzResult<Vec<ployz_types::model::ImageAvailabilityRecord>> {
        self.store.list_image_availability().await
    }
}

#[async_trait]
impl CertificateStore for CountingBackend {
    async fn get_acme_account(&self, issuer_url: &str) -> PloyzResult<Option<AcmeAccountRecord>> {
        self.store.get_acme_account(issuer_url).await
    }

    async fn upsert_acme_account(&self, record: &AcmeAccountRecord) -> PloyzResult<()> {
        self.store.upsert_acme_account(record).await
    }

    async fn list_certificates(&self) -> PloyzResult<Vec<CertificateRecord>> {
        self.store.list_certificates().await
    }

    async fn get_certificate(&self, hostname: &str) -> PloyzResult<Option<CertificateRecord>> {
        self.store.get_certificate(hostname).await
    }

    async fn upsert_certificate(&self, record: &CertificateRecord) -> PloyzResult<()> {
        self.store.upsert_certificate(record).await
    }

    async fn list_acme_challenges(&self) -> PloyzResult<Vec<AcmeChallengeRecord>> {
        self.store.list_acme_challenges().await
    }

    async fn upsert_acme_challenge(&self, record: &AcmeChallengeRecord) -> PloyzResult<()> {
        self.store.upsert_acme_challenge(record).await
    }

    async fn delete_acme_challenge(&self, hostname: &str, token: &str) -> PloyzResult<()> {
        self.store.delete_acme_challenge(hostname, token).await
    }

    async fn upsert_acme_challenge_readiness(
        &self,
        record: &AcmeChallengeReadinessRecord,
    ) -> PloyzResult<()> {
        self.store.upsert_acme_challenge_readiness(record).await
    }

    async fn list_acme_challenge_readiness(
        &self,
        hostname: &str,
        token: &str,
    ) -> PloyzResult<Vec<AcmeChallengeReadinessRecord>> {
        self.store
            .list_acme_challenge_readiness(hostname, token)
            .await
    }

    async fn subscribe_certificates(
        &self,
    ) -> PloyzResult<ployz_store_api::CertificateSubscription> {
        self.store.subscribe_certificates().await
    }

    async fn subscribe_acme_challenges(
        &self,
    ) -> PloyzResult<ployz_store_api::AcmeChallengeSubscription> {
        self.store.subscribe_acme_challenges().await
    }
}

#[async_trait]
impl DeployStore for CountingBackend {
    async fn list_deploy_revisions(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<ServiceRevisionRecord>> {
        self.store.list_deploy_revisions(namespace).await
    }

    async fn list_deploy_releases(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<ServiceReleaseRecord>> {
        self.store.list_deploy_releases(namespace).await
    }

    async fn list_volumes(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<crate::model::VolumeRecord>> {
        self.store.list_volumes(namespace).await
    }

    async fn list_service_branch_lineage(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<ServiceBranchLineageRecord>> {
        self.store.list_service_branch_lineage(namespace).await
    }

    async fn list_volume_movements(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<VolumeMovementRecord>> {
        self.store.list_volume_movements(namespace).await
    }

    async fn list_volume_branches(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<VolumeBranchLineageRecord>> {
        self.store.list_volume_branches(namespace).await
    }

    async fn get_volume(
        &self,
        namespace: &Namespace,
        volume_name: &str,
    ) -> PloyzResult<Option<crate::model::VolumeRecord>> {
        self.store.get_volume(namespace, volume_name).await
    }

    async fn write_deploy_status(&self, deploy: &DeployRecord) -> PloyzResult<()> {
        self.deploy_status_writes.fetch_add(1, Ordering::SeqCst);
        self.deploy_status_records.lock().await.push(deploy.clone());
        if deploy.state == DeployState::Committed {
            let committed_writes = self.committed_status_writes.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_committed_status_writes.load(Ordering::SeqCst)
                || (committed_writes > 1
                    && self
                        .fail_committed_status_writes_after_first
                        .load(Ordering::SeqCst))
            {
                return Err(Error::operation(
                    "counting_write_deploy_status",
                    "injected committed status failure",
                ));
            }
        }
        self.store.write_deploy_status(deploy).await
    }

    async fn commit_deploy(&self, command: &DeployCommit) -> PloyzResult<()> {
        self.commit_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_commit.load(Ordering::SeqCst) {
            return Err(Error::operation(
                "counting_commit_deploy",
                "injected commit failure",
            ));
        }
        self.store.commit_deploy(command).await
    }

    async fn get_deploy(
        &self,
        deploy_id: &DeployId,
    ) -> PloyzResult<Option<crate::model::DeployRecord>> {
        self.store.get_deploy(deploy_id).await
    }

    async fn upsert_deploy_phase(&self, phase: &DeployPhaseRecord) -> PloyzResult<()> {
        if matches!(phase.state, DeployPhaseState::Pending) {
            let call = self.pending_phase_upserts.fetch_add(1, Ordering::SeqCst) + 1;
            if call == self.fail_pending_phase_upsert_on.load(Ordering::SeqCst) {
                return Err(Error::operation(
                    "counting_upsert_deploy_phase",
                    "injected pending phase failure",
                ));
            }
        }
        if matches!(phase.state, DeployPhaseState::Running)
            && self.fail_running_phase_upsert.load(Ordering::SeqCst)
        {
            return Err(Error::operation(
                "counting_upsert_deploy_phase",
                "injected running phase failure",
            ));
        }
        if matches!(phase.state, DeployPhaseState::Succeeded { .. })
            && self.fail_succeeded_phase_upsert.load(Ordering::SeqCst)
        {
            return Err(Error::operation(
                "counting_upsert_deploy_phase",
                "injected succeeded phase failure",
            ));
        }
        self.store.upsert_deploy_phase(phase).await
    }

    async fn get_deploy_phase(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
        phase_id: &DeployPhaseId,
    ) -> PloyzResult<Option<DeployPhaseRecord>> {
        self.store
            .get_deploy_phase(namespace, deploy_id, phase_id)
            .await
    }

    async fn list_deploy_phases(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
    ) -> PloyzResult<Vec<DeployPhaseRecord>> {
        self.store.list_deploy_phases(namespace, deploy_id).await
    }
}

#[async_trait]
impl InstanceStatusStore for CountingBackend {
    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<InstanceStatusRecord>> {
        self.store.list_instance_status(namespace).await
    }

    async fn record_instance_status(&self, record: &InstanceStatusRecord) -> PloyzResult<()> {
        self.store.record_instance_status(record).await
    }

    async fn remove_instance_status(&self, instance_id: &InstanceId) -> PloyzResult<()> {
        self.store.remove_instance_status(instance_id).await
    }
}

impl SyncProbe for CountingBackend {}

impl PeerRttStore for CountingBackend {}

#[async_trait]
impl StoreRuntimeControl for CountingBackend {
    async fn start(&self) -> PloyzResult<()> {
        self.service.start().await
    }

    async fn stop(&self) -> PloyzResult<()> {
        self.service.stop().await
    }

    async fn healthy(&self) -> bool {
        self.service.healthy().await
    }
}
