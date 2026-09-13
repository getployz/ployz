//! Provisioned storage admission through the public planner and SDK error contract.
use super::support::*;
use ployz_core::{RpcError, RpcErrorCode, STORAGE_GIB, StorageBacking, StorageCapacity};
use std::collections::BTreeSet;

fn capacity(free_gib: u64) -> StorageCapacity {
    StorageCapacity {
        backing: StorageBacking::Unallocated {
            host_total_bytes: 80 * STORAGE_GIB,
            host_available_bytes: free_gib * STORAGE_GIB,
        },
        unmanaged_used_bytes: 0,
        volumes: BTreeMap::new(),
    }
}

fn intent() -> DeployIntent {
    let services =
        [("postgres", 10), ("redis", 4), ("data", 30), ("server", 8)].map(|(name, gib)| {
            let mut service = spec(name);
            add_named_volume(&mut service, name);
            make_provisioned(&mut service, name, gib * STORAGE_GIB);
            service
        });
    DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        services.iter(),
        PlanOptions::default(),
    )
}

#[test]
fn complete_plan_reports_all_four_volumes_before_any_deployment() {
    let mut target = machine('1', "machine1");
    target.storage = Some(ployz_core::MachineStorageObservation::Ready);
    let snapshot = DeploySnapshot {
        storage_capacity: BTreeMap::from([(target.machine.id, Ok(capacity(60)))]),
        machines: vec![target],
        ..Default::default()
    };
    let error = preview_deploy(&intent(), &snapshot, IngressContext::default())
        .unwrap_err()
        .into_rpc_error();
    assert_eq!(error.details.get("code").unwrap(), "insufficient_storage");
    assert_eq!(error.details.get("machine").unwrap(), "machine1");
    assert_eq!(
        error.details.get("required_growth_bytes").unwrap(),
        62_491_774_156u64
    );
    assert_eq!(
        error.details.get("shortfall_bytes").unwrap(),
        19_542_101_196u64
    );
    assert!(error.message.contains("18.20 GiB"));
}

#[test]
fn placement_uses_an_alternative_and_unknown_capacity_holds() {
    let mut first = machine('1', "first");
    let mut second = machine('2', "second");
    first.storage = Some(ployz_core::MachineStorageObservation::Ready);
    second.storage = first.storage;
    let mut snapshot = DeploySnapshot {
        storage_capacity: BTreeMap::from([
            (first.machine.id, Ok(capacity(20))),
            (second.machine.id, Ok(capacity(80))),
        ]),
        machines: vec![first, second],
        ..Default::default()
    };
    let preview = preview_deploy(&intent(), &snapshot, IngressContext::default()).unwrap();
    assert_eq!(preview.storage.len(), 1);
    assert_eq!(
        preview.storage.first().unwrap().machine_name.as_str(),
        "second"
    );
    snapshot.storage_capacity.clear();
    let error = preview_deploy(&intent(), &snapshot, IngressContext::default())
        .unwrap_err()
        .into_rpc_error();
    assert_eq!(
        error.details.get("code").unwrap(),
        "storage_capacity_unknown"
    );
}

#[test]
fn surviving_datasets_anchor_single_and_shared_services_without_docker_metadata() {
    for names in [vec!["api"], vec!["api", "worker"]] {
        let mut empty = machine('1', "empty");
        let mut owner = machine('2', "owner");
        empty.storage = Some(ployz_core::MachineStorageObservation::Ready);
        owner.storage = empty.storage;
        let mut existing = capacity(80);
        existing.backing = StorageBacking::Fixed {
            pool_size_bytes: 100 * STORAGE_GIB,
        };
        existing.volumes.insert(
            app_volume("data"),
            ProvisionedVolumeMaximumBytes::new(std::num::NonZeroU64::new(STORAGE_GIB).unwrap()),
        );
        let snapshot = DeploySnapshot {
            storage_capacity: BTreeMap::from([
                (empty.machine.id, Ok(capacity(80))),
                (owner.machine.id, Ok(existing)),
            ]),
            machines: vec![empty, owner],
            ..Default::default()
        };
        let mut services = names
            .iter()
            .map(|name| {
                let mut service = spec(name);
                add_named_volume(&mut service, "data");
                make_provisioned(&mut service, "data", STORAGE_GIB);
                service
            })
            .collect::<Vec<_>>();
        let intent = |services: &[RequestedServiceSpec]| {
            DeployIntent::apply_all(
                ProjectName::parse("app").unwrap(),
                services.iter(),
                PlanOptions::default(),
            )
        };
        let preview =
            preview_deploy(&intent(&services), &snapshot, IngressContext::default()).unwrap();
        assert!(
            preview
                .operations
                .iter()
                .all(|row| row.machine_name.as_ref().unwrap().as_str() == "owner"),
            "{preview:?}"
        );
        assert_eq!(
            preview
                .storage
                .first()
                .unwrap()
                .budget
                .additional_commitment_bytes,
            0
        );
        assert_eq!(preview.volumes_to_create.len(), 1);
        assert_eq!(
            preview.volumes_to_create.first().unwrap().name,
            app_volume("data")
        );
        let mut conflicting = snapshot.clone();
        let owner = conflicting.machines.last().unwrap().machine.id;
        conflicting
            .storage_capacity
            .get_mut(&owner)
            .unwrap()
            .as_mut()
            .unwrap()
            .volumes
            .insert(
                app_volume("data"),
                ProvisionedVolumeMaximumBytes::new(
                    std::num::NonZeroU64::new(2 * STORAGE_GIB).unwrap(),
                ),
            );
        let error = preview_deploy(&intent(&services), &conflicting, IngressContext::default())
            .unwrap_err()
            .into_rpc_error();
        assert_eq!(error.details.get("code").unwrap(), "volume_size_conflict");
        for service in &mut services {
            service.placement.constraints = [label_constraint("empty")].into();
        }
        assert!(
            preview_deploy(&intent(&services), &snapshot, IngressContext::default()).is_err(),
            "must not create empty data on a different Machine"
        );
    }
}

#[test]
fn unknown_dataset_locality_holds_placement_instead_of_creating_elsewhere() {
    let mut owner = machine('1', "unobserved-owner");
    let mut empty = machine('2', "empty");
    owner.storage = Some(ployz_core::MachineStorageObservation::Ready);
    empty.storage = owner.storage;
    let owner_id = owner.machine.id;
    let mut snapshot = DeploySnapshot {
        storage_capacity: BTreeMap::from([(empty.machine.id, Ok(capacity(80)))]),
        machines: vec![owner, empty],
        ..Default::default()
    };
    // Both omitted and failed observations leave possible existing datasets unknown.
    for failure in [
        None,
        Some(RpcError {
            code: RpcErrorCode::Unavailable,
            message: "storage inspection failed".into(),
            details: serde_json::Value::Null,
        }),
    ] {
        if let Some(error) = failure {
            snapshot.storage_capacity.insert(owner_id, Err(error));
        }
        let error = preview_deploy(&intent(), &snapshot, IngressContext::default())
            .unwrap_err()
            .into_rpc_error();
        assert_eq!(
            error.details.get("code").unwrap(),
            "storage_capacity_unknown"
        );
        assert_eq!(error.details.get("machine").unwrap(), "unobserved-owner");
        let mut targeted = intent();
        for service in &mut targeted.target {
            service.placement.constraints = [label_constraint("empty")].into();
        }
        assert!(preview_deploy(&targeted, &snapshot, IngressContext::default()).is_err());
    }
    // A known local dataset may still be reused despite an unrelated inspection failure.
    let known_id = snapshot.machines.last().unwrap().machine.id;
    let known = snapshot
        .storage_capacity
        .get_mut(&known_id)
        .unwrap()
        .as_mut()
        .unwrap();
    known.backing = StorageBacking::Fixed {
        pool_size_bytes: 80 * STORAGE_GIB,
    };
    known.volumes = [("postgres", 10), ("redis", 4), ("data", 30), ("server", 8)]
        .map(|(name, gib)| {
            (
                app_volume(name),
                ProvisionedVolumeMaximumBytes::new(
                    std::num::NonZeroU64::new(gib * STORAGE_GIB).unwrap(),
                ),
            )
        })
        .into();
    let preview = preview_deploy(&intent(), &snapshot, IngressContext::default()).unwrap();
    assert_eq!(
        preview
            .storage
            .first()
            .unwrap()
            .budget
            .additional_commitment_bytes,
        0
    );
}

#[test]
fn placement_budgets_include_observed_pinned_commitments() {
    #[derive(Clone, Copy)]
    enum Usage {
        Single,
        Shared,
        Global,
    }
    for (usage, placement_seed) in [Usage::Single, Usage::Shared, Usage::Global]
        .into_iter()
        .flat_map(|usage| (0..8).map(move |seed| (usage, seed)))
    {
        let mut first = machine('1', "first");
        let mut second = machine('2', "second");
        first.storage = Some(ployz_core::MachineStorageObservation::Ready);
        second.storage = first.storage;
        let mut existing = observed_volume(first.machine.id, "data");
        existing.options = BTreeMap::from([("size".into(), format!("{}b", 30 * STORAGE_GIB))]);
        existing.storage = DockerVolumeStorageObservation::Provisioned {
            mountpoint: MachinePath::parse("/var/lib/ployz-volumes/app_data").unwrap(),
            bound_bytes: std::num::NonZeroU64::new(30 * STORAGE_GIB).unwrap(),
            used_bytes: 0,
        };
        let snapshot = DeploySnapshot {
            // Capacity was collected before Docker discovered the concurrent creation.
            storage_capacity: BTreeMap::from([
                (first.machine.id, Ok(capacity(60))),
                (second.machine.id, Ok(capacity(60))),
            ]),
            machines: vec![first, second],
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![existing]).unwrap(),
            ..Default::default()
        };
        let mut services = vec![("a-owner", "data"), ("z-new", "new")];
        if matches!(usage, Usage::Shared) {
            services.insert(1, ("b-sharer", "data"));
        }
        let services = services
            .into_iter()
            .map(|(name, volume)| {
                let mut service = spec(name);
                add_named_volume(&mut service, volume);
                make_provisioned(&mut service, volume, 30 * STORAGE_GIB);
                if matches!(usage, Usage::Global) && volume == "data" {
                    service.mode = ServiceMode::Global;
                    service.placement.constraints = [label_constraint("first")].into();
                }
                service
            })
            .collect::<Vec<_>>();
        let intent = DeployIntent::apply_all(
            ProjectName::parse("app").unwrap(),
            services.iter(),
            PlanOptions {
                placement_seed,
                ..Default::default()
            },
        );
        let preview = preview_deploy(&intent, &snapshot, IngressContext::default()).unwrap();
        assert_eq!(preview.storage.len(), 2);
        assert_eq!(preview.volumes_to_create.len(), 1);
        assert_eq!(
            preview.volumes_to_create.first().unwrap().name,
            app_volume("new")
        );
        assert!(
            preview
                .storage
                .iter()
                .all(|row| row.budget.requested_bytes == 30 * STORAGE_GIB)
        );
    }
}

#[test]
fn shared_groups_reserve_private_mounts_before_later_placement() {
    for placement_seed in 0..8 {
        let mut first = machine('1', "first");
        let mut second = machine('2', "second");
        first.storage = Some(ployz_core::MachineStorageObservation::Ready);
        second.storage = first.storage;
        let snapshot = DeploySnapshot {
            storage_capacity: BTreeMap::from([
                (first.machine.id, Ok(capacity(60))),
                (second.machine.id, Ok(capacity(60))),
            ]),
            machines: vec![first, second],
            ..Default::default()
        };
        let services = [
            ("a", "shared_a", "private_a"),
            ("b", "shared_b", "private_b"),
        ]
        .into_iter()
        .flat_map(|(group, shared, private)| {
            ["owner", "sharer"].map(|role| {
                let mut service = spec(&format!("{group}-{role}"));
                add_named_volume(&mut service, shared);
                make_provisioned(&mut service, shared, STORAGE_GIB);
                if role == "owner" {
                    add_named_volume(&mut service, private);
                    make_provisioned(&mut service, private, 29 * STORAGE_GIB);
                }
                service
            })
        })
        .collect::<Vec<_>>();
        let intent = DeployIntent::apply_all(
            ProjectName::parse("app").unwrap(),
            services.iter(),
            PlanOptions {
                placement_seed,
                ..Default::default()
            },
        );
        let preview = preview_deploy(&intent, &snapshot, IngressContext::default()).unwrap();
        assert_eq!(preview.storage.len(), 2);
        assert!(
            preview
                .storage
                .iter()
                .all(|row| row.budget.requested_bytes == 30 * STORAGE_GIB)
        );
    }
}

#[test]
fn preparation_and_preview_include_unchanged_assigned_storage() {
    let mut old = spec("old");
    add_named_volume(&mut old, "data");
    make_provisioned(&mut old, "data", 30 * STORAGE_GIB);
    let mut new = spec("new");
    add_named_volume(&mut new, "new");
    make_provisioned(&mut new, "new", STORAGE_GIB);
    let mut target = machine('1', "first");
    target.storage = Some(ployz_core::MachineStorageObservation::Ready);
    let mut existing = observed_volume(target.machine.id, "data");
    existing.options = BTreeMap::from([("size".into(), format!("{}b", 30 * STORAGE_GIB))]);
    existing.storage = DockerVolumeStorageObservation::Provisioned {
        mountpoint: MachinePath::parse("/var/lib/ployz-volumes/app_data").unwrap(),
        bound_bytes: std::num::NonZeroU64::new(30 * STORAGE_GIB).unwrap(),
        used_bytes: 0,
    };
    let snapshot = DeploySnapshot {
        storage_capacity: BTreeMap::from([(target.machine.id, Ok(capacity(60)))]),
        machines: vec![target],
        containers: vec![container('a', '1', &old, &service_id('a'))],
        volume_snapshot: VolumeSnapshot::try_from_observations(vec![existing]).unwrap(),
        ..Default::default()
    };
    let intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [&old, &new],
        PlanOptions::default(),
    );
    let preview = preview_deploy(&intent, &snapshot, IngressContext::default()).unwrap();
    assert!(!preview.operations.iter().any(|row| {
        matches!(&row.operation, DeployOperation::RunContainer { spec, .. } if spec.name.as_str() == "old")
    }));
    let DeployOperation::PrepareVolumes { specs, .. } =
        &preview.operations.first().unwrap().operation
    else {
        panic!("preparation precedes application operations");
    };
    let prepared = specs
        .iter()
        .flat_map(|spec| spec.volume_graph().mounted_provisioned_volumes())
        .map(|volume| {
            let ployz_core::RawVolumeSource::Provisioned {
                name,
                maximum_bytes,
                ..
            } = volume.source.kind()
            else {
                unreachable!("provisioned iterator");
            };
            (name.clone(), maximum_bytes.get())
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        prepared,
        BTreeMap::from([
            (app_volume("data"), 30 * STORAGE_GIB),
            (app_volume("new"), STORAGE_GIB)
        ])
    );
    assert_eq!(
        preview.storage.first().unwrap().budget.requested_bytes,
        prepared.values().sum::<u64>()
    );
    assert_eq!(
        preview
            .volumes_to_create
            .iter()
            .map(|volume| volume.name.clone())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([app_volume("new")])
    );
    let mut recovered = snapshot;
    recovered.volume_snapshot = VolumeSnapshot::default();
    let capacity = recovered
        .storage_capacity
        .values_mut()
        .next()
        .unwrap()
        .as_mut()
        .unwrap();
    capacity.backing = StorageBacking::Fixed {
        pool_size_bytes: 100 * STORAGE_GIB,
    };
    capacity.volumes.insert(
        app_volume("data"),
        ProvisionedVolumeMaximumBytes::new(std::num::NonZeroU64::new(30 * STORAGE_GIB).unwrap()),
    );
    let preview = preview_deploy(&intent, &recovered, IngressContext::default()).unwrap();
    assert_eq!(
        preview
            .volumes_to_create
            .iter()
            .map(|volume| volume.name.clone())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([app_volume("data"), app_volume("new")])
    );
    assert_eq!(
        preview
            .storage
            .first()
            .unwrap()
            .budget
            .additional_commitment_bytes,
        STORAGE_GIB
    );
}
