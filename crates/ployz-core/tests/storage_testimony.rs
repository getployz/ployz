use ployz_core::deploy::{DatasetName, VolumeMaxSizeBytes, VolumeName, ZfsPoolName};
use ployz_core::ids::{MachineId, NamespaceId};
use ployz_core::image::OciPlatform;
use ployz_core::intent::{VolumeKind, VolumePinState};
use ployz_core::machine::{
    MachineContainerObservationSnapshot, MachineDiskSpace, MachineFactsSnapshot,
    MachineStorageTestimony, StorageCapability, StorageUnavailableReason, StrandedVolumeAlarm,
    StrandedVolumeReason, derive_stranded_volume_alarms,
};

#[test]
fn legacy_machine_facts_decode_without_inventing_storage_testimony() {
    let facts = machine_facts(Some(ready("tank")));
    let mut legacy_json = serde_json::to_value(facts).expect("facts serialize");
    legacy_json
        .as_object_mut()
        .expect("facts are an object")
        .remove("storage");

    let decoded =
        serde_json::from_value::<MachineFactsSnapshot>(legacy_json).expect("legacy facts decode");

    assert_eq!(decoded.machine_id(), &machine_id("machine_a"));
    assert_eq!(decoded.disk_space().available_bytes, 512);
    assert_eq!(decoded.storage(), None);
}

#[test]
fn machine_facts_preserve_reported_storage_capability() {
    let facts = machine_facts(Some(StorageCapability::Unavailable {
        reason: StorageUnavailableReason::PoolNotImported { pool: pool("tank") },
    }));

    assert_eq!(
        facts.storage(),
        Some(&StorageCapability::Unavailable {
            reason: StorageUnavailableReason::PoolNotImported { pool: pool("tank") },
        })
    );
}

#[test]
fn container_fact_updates_preserve_storage_testimony() {
    let facts = machine_facts(Some(ready("tank")));

    let updated = facts
        .with_container_replaced(
            ployz_test_support::containers::observation("machine_a", "ctr_a").build(),
            2,
        )
        .expect("container fact update succeeds");

    assert_eq!(updated.storage(), Some(&ready("tank")));
}

#[test]
fn unavailable_storage_alarms_preserve_exact_fault_provenance() {
    let reasons = [
        StorageUnavailableReason::ZfsModuleMissing,
        StorageUnavailableReason::PoolNotImported { pool: pool("tank") },
        StorageUnavailableReason::PoolFaulted { pool: pool("tank") },
    ];

    for (index, reason) in reasons.into_iter().enumerate() {
        let machine = format!("machine_{index}");
        let volume = format!("volume_{index}");
        let alarms = derive_stranded_volume_alarms(
            &[provisioned_pin(&volume, &machine, "tank")],
            &[machine_id(&machine)],
            &[MachineStorageTestimony::new(
                machine_id(&machine),
                Some(StorageCapability::Unavailable {
                    reason: reason.clone(),
                }),
            )],
        );

        assert_eq!(
            alarms,
            vec![alarm(
                &volume,
                &machine,
                StrandedVolumeReason::StorageUnavailable { reason },
            )]
        );
    }
}

#[test]
fn stranded_volume_alarms_distinguish_every_storage_testimony_outcome() {
    let known_machines = vec![
        machine_id("machine_ready"),
        machine_id("machine_silent"),
        machine_id("machine_legacy"),
        machine_id("machine_unprepared"),
        machine_id("machine_unavailable"),
        machine_id("machine_wrong_pool"),
    ];
    let testimony = vec![
        MachineStorageTestimony::new(machine_id("machine_ready"), Some(ready("tank"))),
        MachineStorageTestimony::new(machine_id("machine_legacy"), None),
        MachineStorageTestimony::new(
            machine_id("machine_unprepared"),
            Some(StorageCapability::Unprepared),
        ),
        MachineStorageTestimony::new(
            machine_id("machine_unavailable"),
            Some(StorageCapability::Unavailable {
                reason: StorageUnavailableReason::PoolFaulted { pool: pool("tank") },
            }),
        ),
        MachineStorageTestimony::new(machine_id("machine_wrong_pool"), Some(ready("other"))),
    ];
    let pins = vec![
        provisioned_pin("z_wrong_pool", "machine_wrong_pool", "tank"),
        provisioned_pin("z_ready", "machine_ready", "tank"),
        provisioned_pin("b_legacy", "machine_legacy", "tank"),
        provisioned_pin("a_silent", "machine_silent", "tank"),
        provisioned_pin("c_unprepared", "machine_unprepared", "tank"),
        provisioned_pin("d_unavailable", "machine_unavailable", "tank"),
        VolumePinState::plain(
            namespace_id("default"),
            volume_name("plain"),
            machine_id("machine_silent"),
        ),
    ];

    let alarms = derive_stranded_volume_alarms(&pins, &known_machines, &testimony);

    assert_eq!(
        alarms,
        vec![
            alarm(
                "a_silent",
                "machine_silent",
                StrandedVolumeReason::MachineSilent
            ),
            alarm(
                "b_legacy",
                "machine_legacy",
                StrandedVolumeReason::StorageTestimonyNotReported,
            ),
            alarm(
                "c_unprepared",
                "machine_unprepared",
                StrandedVolumeReason::StorageUnprepared,
            ),
            alarm(
                "d_unavailable",
                "machine_unavailable",
                StrandedVolumeReason::StorageUnavailable {
                    reason: StorageUnavailableReason::PoolFaulted { pool: pool("tank") },
                },
            ),
            alarm(
                "z_wrong_pool",
                "machine_wrong_pool",
                StrandedVolumeReason::PoolMismatch {
                    expected: pool("tank"),
                    reported: pool("other"),
                },
            ),
        ]
    );
}

#[test]
fn stranded_volume_alarms_intersect_pins_with_the_known_machine_set() {
    let pin = provisioned_pin("data", "removed_machine", "tank");

    assert!(derive_stranded_volume_alarms(&[pin], &[], &[]).is_empty());
}

fn ready(pool_name: &str) -> StorageCapability {
    StorageCapability::Ready {
        pool: pool(pool_name),
    }
}

fn machine_facts(storage: Option<StorageCapability>) -> MachineFactsSnapshot {
    let machine_id = machine_id("machine_a");
    MachineFactsSnapshot::try_new(
        machine_id.clone(),
        MachineContainerObservationSnapshot::try_new(machine_id, [])
            .expect("empty container snapshot"),
        None,
        MachineDiskSpace {
            available_bytes: 512,
            total_bytes: 1024,
        },
        storage,
        OciPlatform::current(),
        1,
    )
    .expect("valid machine facts")
}

fn provisioned_pin(volume: &str, machine: &str, pool_name: &str) -> VolumePinState {
    let namespace_id = namespace_id("default");
    let volume_name = volume_name(volume);
    VolumePinState::try_new(
        namespace_id.clone(),
        volume_name.clone(),
        machine_id(machine),
        VolumeKind::Provisioned {
            dataset: DatasetName::for_volume(&pool(pool_name), &namespace_id, &volume_name)
                .expect("canonical dataset"),
            max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("non-zero size"),
        },
    )
    .expect("dataset matches volume")
}

fn alarm(volume: &str, machine: &str, reason: StrandedVolumeReason) -> StrandedVolumeAlarm {
    StrandedVolumeAlarm::new(
        namespace_id("default"),
        volume_name(volume),
        machine_id(machine),
        reason,
    )
}

fn namespace_id(value: &str) -> NamespaceId {
    NamespaceId::try_new(value).expect("namespace id")
}

fn volume_name(value: &str) -> VolumeName {
    VolumeName::try_new(value).expect("volume name")
}

fn machine_id(value: &str) -> MachineId {
    MachineId::try_new(value).expect("machine id")
}

fn pool(value: &str) -> ZfsPoolName {
    ZfsPoolName::try_new(value).expect("pool name")
}
