use ployz_core::{ContainerObservation, DockerVolume, MachineObservation, PartialResult, RpcError};

use crate::connect::{Client, ConnectError};

use super::{DeploySnapshot, ObservedDockerVolume};

pub(crate) struct DeploySnapshotGather {
    pub snapshot: DeploySnapshot,
    pub containers: PartialResult<Vec<ContainerObservation>, RpcError>,
    pub volumes: PartialResult<Vec<DockerVolume>, RpcError>,
}

impl Client {
    /// Gather an observer-relative Deploy Snapshot from the given Machines.
    /// Container and volume fan-out failures stay in the returned Partial
    /// Results; the snapshot keeps successful observations.
    pub(crate) async fn deploy_snapshot(
        &mut self,
        machines: Vec<MachineObservation>,
    ) -> Result<DeploySnapshotGather, ConnectError> {
        let containers = self.live_services_from(&machines).await?.containers;
        let volumes = self.list_volumes(&machines).await;
        let snapshot = snapshot_from_partial(machines, &containers, &volumes);
        Ok(DeploySnapshotGather {
            snapshot,
            containers,
            volumes,
        })
    }
}

fn snapshot_from_partial(
    machines: Vec<MachineObservation>,
    containers: &PartialResult<Vec<ContainerObservation>, RpcError>,
    volumes: &PartialResult<Vec<DockerVolume>, RpcError>,
) -> DeploySnapshot {
    DeploySnapshot {
        machines,
        containers: containers
            .successes
            .iter()
            .flat_map(|success| success.value.iter().cloned())
            .collect(),
        volumes: volumes
            .successes
            .iter()
            .flat_map(|success| success.value.iter().cloned())
            .map(|volume| ObservedDockerVolume {
                id: volume.id,
                driver: volume.driver,
                options: volume.options,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::Ipv6Addr;

    use ployz_core::{
        AdvertisedEndpoint, ContainerId, ContainerKind, ContainerObservation,
        ContainerRuntimeObservation, DockerVolume, DockerVolumeId, DockerVolumeName,
        HealthObservation, Machine, MachineFailure, MachineId, MachineName, MachineObservation,
        MachineSubnet, MachineSuccess, ManagementAddress, MembershipObservation, RpcError,
        RpcErrorCode, ServiceId, ServiceName, WireGuardPublicKey,
    };
    use serde_json::{Value, json};

    use super::*;

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
            service_id: service_id.clone(),
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
}
