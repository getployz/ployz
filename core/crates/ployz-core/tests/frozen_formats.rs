//! Today's types still read what a 0.2.0 daemon stored and spoke, and write it
//! back without dropping a field. The fixtures are frozen; see `frozen/mod.rs`.
#![expect(
    clippy::indexing_slicing,
    reason = "Frozen JSON fixtures are indexed by their known fields."
)]

mod frozen;

use frozen::assert_retains;
use ployz_core::{
    CertificateKeyType, CertificatePolicy, ContainerObservation, DockerVolume,
    EnrollmentAssignment, Machine, OpaquePayload, RpcResponseBody, resolve_certificate_policy,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::time::Duration;

macro_rules! fixture {
    ($path:literal) => {
        include_str!(concat!("fixtures/v0.2.0/", $path))
    };
}

fn reads_and_retains<T: DeserializeOwned + Serialize>(name: &str, text: &str) -> T {
    let frozen: Value = serde_json::from_str(text).unwrap();
    let value: T = serde_json::from_value(frozen.clone())
        .unwrap_or_else(|error| panic!("{name}: 0.2.0 value no longer reads: {error}"));
    assert_retains(&frozen, &serde_json::to_value(&value).unwrap(), name);
    value
}

#[test]
fn replicated_store_bodies_from_0_2_0_still_read() {
    reads_and_retains::<Machine>("machines.info", fixture!("store/machines.json"));
    reads_and_retains::<ContainerObservation>(
        "containers.container",
        fixture!("store/containers.json"),
    );
    reads_and_retains::<DockerVolume>("volumes.volume", fixture!("store/volumes.json"));
}

#[test]
fn replicated_cluster_values_from_0_2_0_still_read() {
    let cluster: Value = serde_json::from_str(fixture!("store/cluster.json")).unwrap();
    cluster["network"]
        .as_str()
        .unwrap()
        .parse::<ipnet::Ipv4Net>()
        .unwrap();
    let policy = resolve_certificate_policy(
        cluster["certificate_policy"].as_str(),
        &CertificatePolicy::built_in(None),
    )
    .unwrap();
    assert_eq!(policy.key_type(), &CertificateKeyType::EcdsaP384);
    assert_eq!(policy.probe_timeout(), Duration::from_secs(10));
}

#[test]
fn machine_rpc_exchanges_from_0_2_0_still_decode() {
    for (name, text) in [
        ("describe_contract", fixture!("rpc/describe_contract.json")),
        ("machine_token", fixture!("rpc/machine_token.json")),
        ("initialize", fixture!("rpc/initialize.json")),
        ("register", fixture!("rpc/register.json")),
        ("join", fixture!("rpc/join.json")),
        ("list_machines", fixture!("rpc/list_machines.json")),
        ("update_machine", fixture!("rpc/update_machine.json")),
        ("list_containers", fixture!("rpc/list_containers.json")),
        ("inspect_container", fixture!("rpc/inspect_container.json")),
    ] {
        let exchange: Value = serde_json::from_str(text).unwrap();
        let wire = |value: &Value| OpaquePayload::new(serde_json::to_vec(value).unwrap());

        let request = wire(&exchange["request"])
            .decode_request()
            .unwrap_or_else(|error| panic!("{name} request: {error}"));
        assert_eq!(request.body.command(), name);
        let reencoded: Value = request.encode().unwrap().decode_json().unwrap();
        assert_retains(&exchange["request"], &reencoded, name);

        let response = wire(&exchange["response"])
            .decode_response()
            .unwrap_or_else(|error| panic!("{name} response: {error}"));
        assert!(
            !matches!(response.body, RpcResponseBody::Unknown { .. }),
            "{name}: 0.2.0 response kind is no longer known"
        );
        let reencoded: Value = response.encode().unwrap().decode_json().unwrap();
        assert_retains(&exchange["response"], &reencoded, name);
    }
}

#[test]
fn saved_enrollment_assignment_from_0_2_0_still_reads() {
    reads_and_retains::<EnrollmentAssignment>(
        "enrollment assignment",
        fixture!("enrollment/assignment.json"),
    );
}
