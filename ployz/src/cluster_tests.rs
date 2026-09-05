use std::collections::BTreeMap;
use std::net::Ipv6Addr;

use ployz_core::{
    AdvertisedEndpoint, ContainerRuntimeObservation, DockerVolumeId, DockerVolumeName,
    DockerVolumeStorageObservation, HealthObservation, Machine, MachinePath, ManagementAddress,
    MembershipObservation, ProjectName, ServiceId, ServiceName, WireGuardPublicKey,
};
use serde_json::{Value, json};

use super::*;

#[test]
fn unspecified_or_negative_stop_timeout_has_no_rpc_deadline() {
    assert_eq!(stop_rpc_timeout(Some(-1)), None);
    assert_eq!(stop_rpc_timeout(None), None);
    assert_eq!(
        stop_rpc_timeout(Some(5)),
        Some(TARGET_RPC_TIMEOUT + Duration::from_secs(5))
    );
}

#[test]
fn remove_tolerates_a_missing_preliminary_stop_target() {
    let missing = RpcError {
        code: RpcErrorCode::NotFound,
        message: "gone".into(),
        details: Value::Null,
    };

    assert!(accept_stop_result(ContainerAction::Remove, Err(missing.clone())).is_ok());
    assert_eq!(
        accept_stop_result(ContainerAction::Stop, Err(missing.clone()))
            .unwrap_err()
            .code,
        RpcErrorCode::NotFound
    );
    assert_eq!(
        accept_stop_result(
            ContainerAction::Remove,
            Err(RpcError {
                code: RpcErrorCode::Internal,
                ..missing
            })
        )
        .unwrap_err()
        .code,
        RpcErrorCode::Internal
    );
}

#[test]
fn deploy_snapshot_keeps_successful_observations_and_query_gaps() {
    let machines = vec![machine('a'), machine('b')];
    let container = observation('1', 'a');
    let mut volume = docker_volume('a', "data");
    volume.storage = DockerVolumeStorageObservation::Provisioned {
        mountpoint: MachinePath::parse("/var/lib/ployz-volumes/data").unwrap(),
        bound_bytes: std::num::NonZeroU64::new(1_073_741_824).unwrap(),
        used_bytes: 42,
    };
    let containers = PartialResult {
        successes: vec![MachineSuccess {
            machine_id: machine_id('a'),
            value: vec![container.clone()],
        }],
        failures: vec![MachineFailure {
            machine_id: machine_id('b'),
            error: unavailable("container listing failed"),
        }],
        omissions: vec![machine_id('c')],
    };
    let volumes = PartialResult {
        successes: vec![MachineSuccess {
            machine_id: machine_id('a'),
            value: VolumeInventory {
                volumes: vec![volume.clone()],
                failures: vec![ployz_core::VolumeObservationFailure {
                    id: ployz_core::DockerVolumeId {
                        machine_id: machine_id('a'),
                        name: DockerVolumeName::parse("unavailable").unwrap(),
                    },
                    error: unavailable("volume detail failed"),
                }],
            },
        }],
        failures: vec![MachineFailure {
            machine_id: machine_id('b'),
            error: unavailable("volume listing failed"),
        }],
        omissions: Vec::new(),
    };
    let expected_container_failures = containers.failures.clone();
    let expected_container_omissions = containers.omissions.clone();
    let snapshot =
        snapshot_from_partial(machines.clone(), containers, volumes, BTreeMap::new()).unwrap();

    assert_eq!(snapshot.machines, machines);
    assert_eq!(snapshot.containers, [container]);
    assert_eq!(snapshot.volume_snapshot.observations(), [volume]);
    assert!(snapshot.volume_snapshot.listing_warnings().any(|message| {
        message.contains(&machine_id('a').to_string())
            && message.contains("Docker Volume unavailable")
    }));
    assert_eq!(snapshot.container_failures, expected_container_failures);
    assert_eq!(snapshot.container_omissions, expected_container_omissions);
    assert_eq!(
        snapshot
            .volume_snapshot
            .machine_gap(machine_id('b'))
            .as_deref(),
        Some("Docker Volume inventory failed: volume listing failed")
    );
    assert!(!snapshot.is_observer_complete());
}

#[test]
fn targeted_volume_inventory_rejects_unsafe_routing_evidence() {
    let target = machine_id('a');
    let failure = |machine, name: &str| ployz_core::VolumeObservationFailure {
        id: DockerVolumeId {
            machine_id: machine_id(machine),
            name: DockerVolumeName::parse(name).unwrap(),
        },
        error: unavailable("inspect failed"),
    };

    for inventory in [
        VolumeInventory {
            volumes: vec![docker_volume('b', "wrong-machine")],
            failures: Vec::new(),
        },
        VolumeInventory {
            volumes: Vec::new(),
            failures: vec![failure('b', "wrong-machine")],
        },
        VolumeInventory {
            volumes: vec![
                docker_volume('a', "duplicate"),
                docker_volume('a', "duplicate"),
            ],
            failures: Vec::new(),
        },
        VolumeInventory {
            volumes: vec![docker_volume('a', "contradiction")],
            failures: vec![failure('a', "contradiction")],
        },
    ] {
        assert_eq!(
            validate_volume_inventory(target, inventory)
                .expect_err("unsafe inventory is rejected before MachineSuccess")
                .code,
            RpcErrorCode::Internal
        );
    }

    assert!(
        validate_volume_inventory(
            target,
            VolumeInventory {
                volumes: vec![docker_volume('a', "observed")],
                failures: vec![failure('a', "unavailable")],
            },
        )
        .is_ok()
    );
}

#[test]
fn volume_snapshot_rejects_duplicate_and_contradictory_evidence() {
    let volume = docker_volume('a', "data");
    let named_failure = || ployz_core::VolumeObservationFailure {
        id: volume.id.clone(),
        error: unavailable("inspect failed"),
    };
    let machine_failure = || MachineFailure {
        machine_id: machine_id('a'),
        error: unavailable("list failed"),
    };

    for result in [
        VolumeSnapshot::try_from_observations(vec![volume.clone(), volume.clone()]),
        VolumeSnapshot::try_from_parts(
            vec![volume.clone()],
            vec![named_failure()],
            Vec::new(),
            Vec::new(),
        ),
        VolumeSnapshot::try_from_parts(
            Vec::new(),
            vec![named_failure(), named_failure()],
            Vec::new(),
            Vec::new(),
        ),
        VolumeSnapshot::try_from_parts(
            Vec::new(),
            Vec::new(),
            vec![machine_failure(), machine_failure()],
            Vec::new(),
        ),
        VolumeSnapshot::try_from_parts(
            Vec::new(),
            Vec::new(),
            vec![machine_failure()],
            vec![machine_id('a')],
        ),
        VolumeSnapshot::try_from_parts(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![machine_id('a'), machine_id('a')],
        ),
        VolumeSnapshot::try_from_parts(
            vec![volume.clone()],
            Vec::new(),
            Vec::new(),
            vec![machine_id('a')],
        ),
    ] {
        assert_eq!(
            result
                .expect_err("invalid Volume Snapshot evidence is rejected")
                .code,
            RpcErrorCode::InvalidArgument
        );
    }

    let partial = PartialResult {
        successes: vec![MachineSuccess {
            machine_id: machine_id('a'),
            value: VolumeInventory {
                volumes: vec![volume],
                failures: Vec::new(),
            },
        }],
        failures: vec![machine_failure()],
        omissions: Vec::new(),
    };
    assert_eq!(
        VolumeSnapshot::from_partial(partial).unwrap_err().code,
        RpcErrorCode::Internal
    );
}

fn machine(hex: char) -> MachineObservation {
    MachineObservation::new(
        Machine {
            id: machine_id(hex),
            name: MachineName::parse(format!("machine-{hex}")).unwrap(),
            subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
                .parse()
                .unwrap(),
            management_address: ManagementAddress(Ipv6Addr::LOCALHOST),
            public_key: WireGuardPublicKey([hex as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::<AdvertisedEndpoint>::new(),
            runtime: Default::default(),
        },
        MembershipObservation::Up,
    )
}

fn machine_id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

fn observation(id: char, machine: char) -> ContainerObservation {
    let service_id = ServiceId::parse(id.to_string().repeat(32)).unwrap();
    let service_name = ServiceName::parse("api").unwrap();
    ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
        container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
        display_name: "api".into(),
        created_at_unix_nanos: 0,
        machine_id: machine_id(machine),
        project_name: ProjectName::parse("app").unwrap(),
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec: serde_json::from_value(json!({
            "service_id": service_id,
            "name": service_name,
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "alpine:3.23.3", "pull_policy": "missing" }
        }))
        .unwrap(),
        address: None,
        labels: BTreeMap::new(),
    })
    .unwrap()
}

fn docker_volume(machine: char, name: &str) -> DockerVolume {
    DockerVolume {
        id: DockerVolumeId {
            machine_id: machine_id(machine),
            name: DockerVolumeName::parse(name).unwrap(),
        },
        options: BTreeMap::from([("type".into(), "none".into())]),
        labels: BTreeMap::from([("keep".into(), "out".into())]),
        storage: ployz_core::DockerVolumeStorageObservation::Plain {
            driver: "local".into(),
        },
    }
}

fn unavailable(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message: message.into(),
        details: Value::Null,
    }
}
