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
use crate::deploy::participant::{DeployParticipantClient, StartCandidateRequest};
use crate::error::{DeployError, Error, Result};
use crate::model::RegionRole;
use crate::model::{
    AcmeAccountRecord, AcmeChallengeReadinessRecord, AcmeChallengeRecord, CertificateRecord,
    DeployChangeKind, DeployId, DeployRecord, DeployState, DrainState, InstanceId, InstancePhase,
    InstanceStatusRecord, MachineId, MachineLifecycle, MachineMembership, MachineTopology,
    OverlayIp, PublicKey, ServiceBranchLineageRecord, ServiceRelease, ServiceReleaseRecord,
    ServiceReleaseSlot, ServiceRevisionRecord, ServiceRoutingPolicy, SlotId, VolumeRecord,
};
use async_trait::async_trait;
use ployz_store_api::memory::{MemoryService, MemoryStore};
use ployz_store_api::{
    CertificateStore, DeployCommit, DeployStore, InstanceStatusStore, InviteStore,
    MachineMembershipStore, MachineSubscription, PeerRttStore, RoutingEventSubscription,
    RoutingStateStore, StoreDriver, StoreRuntimeControl, SyncProbe,
};
use ployz_types::Result as PloyzResult;
use ployz_types::spec::{
    ContainerSpec, DeployIntent, DeployManifest, HttpRoute, Mount, MountSource, Namespace,
    NetworkMode, Placement, PortProtocol, PullPolicy, Resources, RestartPolicy, RolloutStrategy,
    RouteSpec, ServiceIntent, ServiceIntentHint, ServicePort, ServiceSpec, VolumeDeclaration,
    VolumeScope,
};
use std::collections::{BTreeMap, HashMap};
use std::net::Ipv6Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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
fn replicated_slot_stays_on_draining_machine_instead_of_replacing_lifecycle_truth() {
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
    assert_eq!(desired[0].machine_id, MachineId("machine-b".into()));
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
    // deploying -> post-cert warning refresh -> cleanup_pending
    assert_eq!(backend.deploy_status_write_count(), 3);
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
    fail_remove_instance: Option<String>,
    open_active: Arc<AtomicUsize>,
    max_open: Arc<AtomicUsize>,
    start_count: Arc<AtomicUsize>,
    start_active: Arc<AtomicUsize>,
    max_global_start: Arc<AtomicUsize>,
    drain_count: Arc<AtomicUsize>,
    remove_count: Arc<AtomicUsize>,
    machine_state: Arc<std::sync::Mutex<HashMap<String, usize>>>,
    machine_max: Arc<std::sync::Mutex<HashMap<String, usize>>>,
    start_log_entries: Arc<Mutex<Vec<String>>>,
    start_requests: Arc<Mutex<Vec<StartCandidateRequest>>>,
}

impl FakeController {
    fn max_open_seen(&self) -> usize {
        self.max_open.load(Ordering::SeqCst)
    }

    fn start_count(&self) -> usize {
        self.start_count.load(Ordering::SeqCst)
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

    async fn start_requests(&self) -> Vec<StartCandidateRequest> {
        self.start_requests.lock().await.clone()
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
    }

    fn should_fail_start(&self, service: &str) -> bool {
        self.fail_start_service.as_deref() == Some(service)
    }

    async fn on_start_end(&self, machine_id: &MachineId) {
        self.start_active.fetch_sub(1, Ordering::SeqCst);
        let mut machine_state = self.machine_state.lock().expect("machine state lock");
        let Some(current) = machine_state.get_mut(&machine_id.0) else {
            return;
        };
        *current -= 1;
    }

    fn on_drain(&self) {
        self.drain_count.fetch_add(1, Ordering::SeqCst);
    }

    fn on_remove(&self) {
        self.remove_count.fetch_add(1, Ordering::SeqCst);
    }

    fn should_fail_remove(&self, instance_id: &InstanceId) -> bool {
        self.fail_remove_instance.as_deref() == Some(instance_id.0.as_str())
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
        Ok(Vec::new())
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

    async fn drain_instance(
        &self,
        _machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        _instance_id: &InstanceId,
    ) -> Result<()> {
        self.controller.on_drain();
        Ok(())
    }

    async fn remove_instance(
        &self,
        _machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        instance_id: &InstanceId,
    ) -> Result<()> {
        self.controller.on_remove();
        if self.controller.should_fail_remove(instance_id) {
            return Err(ployz_types::error::Error::operation(
                "fake_remove",
                format!("injected remove failure for '{}'", instance_id),
            ));
        }
        Ok(())
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
    );
    for machine_id in machine_ids {
        store
            .upsert_self_machine(&test_machine(machine_id, MachineLifecycle::Active))
            .await
            .expect("seed machine");
    }
    (store, backend)
}

fn test_manifest(services: Vec<ServiceSpec>) -> DeployManifest {
    DeployManifest {
        namespace: Namespace("test".into()),
        intent: None,
        volumes: Vec::new(),
        services,
    }
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
    let deploy_id = DeployId(format!("seed-{volume_name}"));
    let volume = VolumeRecord {
        namespace: namespace.clone(),
        volume_name: volume_name.into(),
        scope: VolumeScope::Single,
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
    deploy_status_records: Mutex<Vec<DeployRecord>>,
}

impl CountingBackend {
    fn new() -> Self {
        Self {
            store: Arc::new(MemoryStore::new()),
            service: Arc::new(MemoryService::new()),
            commit_calls: AtomicUsize::new(0),
            deploy_status_writes: AtomicUsize::new(0),
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
    }

    async fn last_deploy_status_write(&self) -> Option<DeployRecord> {
        self.deploy_status_records.lock().await.last().cloned()
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
        self.store.write_deploy_status(deploy).await
    }

    async fn commit_deploy(&self, command: &DeployCommit) -> PloyzResult<()> {
        self.commit_calls.fetch_add(1, Ordering::SeqCst);
        self.store.commit_deploy(command).await
    }

    async fn get_deploy(
        &self,
        deploy_id: &DeployId,
    ) -> PloyzResult<Option<crate::model::DeployRecord>> {
        self.store.get_deploy(deploy_id).await
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
