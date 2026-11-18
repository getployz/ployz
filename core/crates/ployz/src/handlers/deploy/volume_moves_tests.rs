use ployz_core::{Machine, MachineId, MachineName, Namespace, RequestedServiceSpec, ServiceName};
use ployz_store::VolumeName;
use serde_json::json;

use super::called_for;

fn server(name: &str, region: &str) -> Machine {
    Machine {
        labels: [("region".parse().unwrap(), region.parse().unwrap())].into(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: MachineId::parse(name[name.len() - 1..].repeat(32)).unwrap(),
        name: MachineName::parse(name).unwrap(),
        subnet: "10.210.1.0/24".parse().unwrap(),
        public_key: ployz_core::WireGuardPublicKey([1; 32]),
        public_ip: None,
        advertised_endpoints: Vec::new(),
        runtime: Default::default(),
        join_approval: None,
        build_concurrency: None,
    }
}

fn postgres(constraints: &[&str]) -> RequestedServiceSpec {
    serde_json::from_value(json!({
        "name": "postgres",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "postgres:17", "pull_policy": "missing" },
        "placement": { "constraints": constraints },
    }))
    .unwrap()
}

#[test]
fn this_device_signs_a_move_only_off_a_server_its_deploy_leaves_out_onto_one_it_takes() {
    let namespace = Namespace::parse("shop").unwrap();
    let volume = VolumeName::parse("pg-data").unwrap();
    let service = ServiceName::parse("postgres").unwrap();
    let (syd, fsn, fsn2) = (
        server("syd-1", "syd"),
        server("fsn-1", "fsn"),
        server("fsn-2", "fsn"),
    );
    let in_fsn = [postgres(&["node.labels.region==fsn"])];
    let asked = |specs: &[RequestedServiceSpec], writer, target| {
        called_for(specs, &namespace, &volume, &service, (writer, target))
            .map_err(|error| error.to_string())
    };
    assert_eq!(asked(&in_fsn, &syd, &fsn), Ok(()));
    // Cloud names the target, so one outside the region this Device approved is refused.
    assert_eq!(
        asked(&in_fsn, &fsn, &syd),
        Err("This deploy doesn't place postgres on syd-1.".to_owned())
    );
    assert_eq!(
        asked(&in_fsn, &fsn, &fsn2),
        Err("This deploy can run postgres on fsn-1, where pg-data is.".to_owned())
    );
    assert_eq!(
        asked(&[postgres(&[])], &syd, &fsn),
        Err("This deploy can run postgres on syd-1, where pg-data is.".to_owned())
    );
    assert_eq!(
        asked(&[], &syd, &fsn),
        Err("This deploy doesn't include postgres, which uses pg-data.".to_owned())
    );
}
