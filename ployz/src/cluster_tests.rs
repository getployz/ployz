use std::collections::BTreeMap;
use std::net::Ipv6Addr;

use ployz_core::{
    AdvertisedEndpoint, ContainerRuntimeObservation, DockerVolumeId, DockerVolumeName,
    HealthObservation, Machine, MachineSubnet, ManagementAddress, ServiceId, ServiceName,
    WireGuardPublicKey,
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
fn deploy_snapshot_keeps_successful_observations_and_drops_failures() {
    let machines = vec![machine('a'), machine('b')];
    let container = observation('1', 'a');
    let volume = docker_volume('a', "data");
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
            value: vec![volume.clone()],
        }],
        failures: vec![MachineFailure {
            machine_id: machine_id('b'),
            error: unavailable("volume listing failed"),
        }],
        omissions: Vec::new(),
    };
    let snapshot = snapshot_from_partial(machines.clone(), &containers, &volumes);

    assert_eq!(snapshot.machines, machines);
    assert_eq!(snapshot.containers, [container]);
    assert_eq!(
        snapshot.volumes,
        [ObservedDockerVolume {
            id: volume.id,
            driver: volume.driver,
            options: volume.options,
        }]
    );
}

fn machine(hex: char) -> MachineObservation {
    MachineObservation {
        machine: Machine {
            id: machine_id(hex),
            name: MachineName::parse(format!("machine-{hex}")).unwrap(),
            subnet: MachineSubnet(
                format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
                    .parse()
                    .unwrap(),
            ),
            management_address: ManagementAddress(Ipv6Addr::LOCALHOST),
            public_key: WireGuardPublicKey([hex as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::<AdvertisedEndpoint>::new(),
            runtime: Default::default(),
        },
        membership: MembershipObservation::Up,
        selected_endpoint: None,
    }
}

fn machine_id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

fn observation(id: char, machine: char) -> ContainerObservation {
    let service_id = ServiceId::parse(id.to_string().repeat(32)).unwrap();
    let service_name = ServiceName::parse("api").unwrap();
    ContainerObservation {
        container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
        display_name: "api".into(),
        created_at_unix_nanos: 0,
        machine_id: machine_id(machine),
        service_id,
        service_name: service_name.clone(),
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
    }
}

fn docker_volume(machine: char, name: &str) -> DockerVolume {
    DockerVolume {
        id: DockerVolumeId {
            machine_id: machine_id(machine),
            name: DockerVolumeName::parse(name).unwrap(),
        },
        driver: "local".into(),
        options: BTreeMap::from([("type".into(), "none".into())]),
        labels: BTreeMap::from([("keep".into(), "out".into())]),
    }
}

fn unavailable(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message: message.into(),
        details: Value::Null,
    }
}
