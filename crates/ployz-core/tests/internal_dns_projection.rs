use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use base64::Engine as _;
use ployz_core::corrosion::StoredRow;
use ployz_core::ids::{ClusterId, MachineId, MachineRowId, NamespaceId, ServiceId};
use ployz_core::ingress::{AutomaticHostnameConfiguration, PloyzDnsTargetIntent};
use ployz_core::intent::{ActiveMachineState, IntentSnapshot};
use ployz_core::machine::MachineName;
use ployz_core::machine::runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, MachineFactsSnapshot,
    ManagedContainerKind,
};
use ployz_core::machine::{InstallRolePolicy, MachineLifecycle};
use ployz_core::network::MachineEndpointSubnet;
use ployz_core::network::internal_dns::{
    INTERNAL_DNS_READINESS_NAME, InternalDnsRowProjectionError, InternalDnsRowProjectionInput,
    InternalDnsSearchDomain, InternalServiceName, internal_dns_records, project_internal_dns_rows,
};
use ployz_test_support::fixtures::serving_target_entry;
use ployz_test_support::ids::{machine_id, operation_id};
use ployz_test_support::{containers, fixtures};
use sha2::{Digest, Sha256};

const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const LOCAL_MACHINE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const REMOTE_MACHINE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const WRONG_PROVIDER_MACHINE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
const NAMESPACE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
const SHADOW_NAMESPACE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB0";
const SERVICE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB1";
const SHADOW_SERVICE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB2";
const ACTIVE_DEPLOY: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB3";
const OLD_DEPLOY: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB4";
const PEER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB5";
const EMPTY_SERVICE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB6";

#[test]
fn corrosion_rows_project_local_bind_and_cluster_wide_active_service_records() {
    let projection = project_internal_dns_rows(projection_input()).expect("row projection");
    let name = InternalServiceName::try_from_labels("api", "prod").expect("record name");
    let empty_name = InternalServiceName::try_from_labels("worker", "prod").expect("record name");

    assert_eq!(
        projection.bind,
        SocketAddr::from((Ipv4Addr::new(10, 210, 20, 1), 53))
    );
    assert_eq!(
        projection.records,
        BTreeMap::from([
            (
                name,
                vec![Ipv4Addr::new(10, 210, 20, 8), Ipv4Addr::new(10, 210, 30, 9)],
            ),
            (empty_name, Vec::new()),
        ])
    );
}

#[test]
fn corrosion_rows_cannot_project_the_reserved_readiness_record() {
    let mut input = projection_input();
    const READINESS_NAMESPACE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB7";
    const READINESS_SERVICE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB8";
    input
        .namespace_rows
        .push(namespace_row(READINESS_NAMESPACE, "ployz"));
    input.service_rows.push(service_row(
        READINESS_SERVICE,
        READINESS_NAMESPACE,
        "readiness",
        ACTIVE_DEPLOY,
    ));
    input.container_rows.push(container_row(
        "container-readiness",
        LOCAL_MACHINE,
        READINESS_SERVICE,
        READINESS_NAMESPACE,
        "10.210.20.53",
        ACTIVE_DEPLOY,
    ));

    let projection = project_internal_dns_rows(input).expect("row projection");

    assert!(
        projection
            .records
            .keys()
            .all(|name| name.as_str() != INTERNAL_DNS_READINESS_NAME)
    );
}

#[test]
fn corrosion_row_projection_reports_missing_cluster_and_local_machine() {
    let mut missing_cluster = projection_input();
    missing_cluster.cluster_rows.clear();
    assert_eq!(
        project_internal_dns_rows(missing_cluster),
        Err(InternalDnsRowProjectionError::MissingCluster {
            cluster_id: cluster_id(),
        })
    );

    let mut malformed_cluster = projection_input();
    malformed_cluster.cluster_rows = vec![StoredRow::new(CLUSTER, "{")];
    assert_eq!(
        project_internal_dns_rows(malformed_cluster),
        Err(InternalDnsRowProjectionError::InvalidCluster {
            cluster_id: cluster_id(),
        })
    );

    let mut missing_machine = projection_input();
    missing_machine.local_machine_id =
        MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FB7").expect("machine id");
    assert_eq!(
        project_internal_dns_rows(missing_machine),
        Err(InternalDnsRowProjectionError::LocalMachineMissing {
            machine_id: MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FB7").expect("machine id"),
        })
    );
}

#[test]
fn corrosion_row_projection_retains_a_dark_draining_machine_without_liveness_testimony() {
    let mut input = projection_input();
    input
        .container_rows
        .retain(|row| row.key == "container-remote-dark" || row.key == "container-old-deploy");
    input
        .service_rows
        .retain(|row| row.key == SERVICE || row.key == SHADOW_SERVICE);

    let projection = project_internal_dns_rows(input).expect("row projection");
    let name = InternalServiceName::try_from_labels("api", "prod").expect("record name");

    assert_eq!(
        projection.records,
        BTreeMap::from([(name, vec![Ipv4Addr::new(10, 210, 30, 9)])])
    );
}

#[test]
fn internal_service_name_from_labels_rejects_non_dns_labels() {
    assert!(InternalServiceName::try_from_labels("api_v2", "prod").is_err());
    assert!(InternalServiceName::try_from_labels("-api", "prod").is_err());
    assert!(InternalServiceName::try_from_labels("api", "prod-").is_err());
    assert!(InternalServiceName::try_from_labels("Api", "prod").is_err());
    assert!(InternalServiceName::try_from_labels("api", "Prod").is_err());
}

#[test]
fn internal_dns_search_domain_uses_the_human_namespace_label() {
    let domain = InternalDnsSearchDomain::try_from_namespace_label("prod-east")
        .expect("human namespace label");

    assert_eq!(domain.as_str(), "prod-east.internal");
    assert!(InternalDnsSearchDomain::try_from_namespace_label("Prod-East").is_err());
}

#[test]
fn internal_dns_projection_returns_sorted_unique_running_service_ipv4_addresses() {
    let observations = [
        containers::observation("machine_a", "ctr_1")
            .with(containers::identity("db").namespace("default"))
            .running_at(IpAddr::V4(Ipv4Addr::new(10, 198, 2, 8)))
            .build(),
        containers::observation("machine_a", "ctr_2")
            .with(containers::identity("db").namespace("default"))
            .running_at(IpAddr::V4(Ipv4Addr::new(10, 198, 1, 9)))
            .build(),
        containers::observation("machine_a", "ctr_3")
            .with(containers::identity("db").namespace("default"))
            .running_at(IpAddr::V4(Ipv4Addr::new(10, 198, 2, 8)))
            .build(),
        containers::observation("machine_a", "ctr_job")
            .with(
                containers::identity("db")
                    .namespace("default")
                    .kind(ManagedContainerKind::Job),
            )
            .running_at(IpAddr::V4(Ipv4Addr::new(10, 198, 3, 10)))
            .build(),
        ployz_core::machine::ManagedContainerObservation {
            state: ContainerRuntimeState::Exited,
            ..containers::observation("machine_a", "ctr_exited")
                .with(containers::identity("db").namespace("default"))
                .build()
        },
    ];
    let machine_id = MachineId::try_new("machine_a").expect("machine id");
    let facts = MachineFactsSnapshot::try_new(
        machine_id.clone(),
        MachineContainerObservationSnapshot::try_new(machine_id, observations)
            .expect("container facts"),
        None,
        ployz_test_support::fixtures::test_disk_space(),
        None,
        ployz_core::image::OciPlatform::current(),
        1,
    )
    .expect("machine facts");
    let name = InternalServiceName::try_from_ids(
        &ServiceId::try_new("db").expect("service id"),
        &NamespaceId::try_new("default").expect("namespace id"),
    )
    .expect("internal service name");

    assert_eq!(
        internal_dns_records(&intent(["machine_a"], "entry_test"), &[facts]),
        BTreeMap::from([(
            name,
            vec![Ipv4Addr::new(10, 198, 1, 9), Ipv4Addr::new(10, 198, 2, 8)]
        )])
    );
}

#[test]
fn internal_dns_projection_excludes_facts_from_removed_machines() {
    let facts = machine_facts(
        "machine_removed",
        [containers::observation("machine_removed", "ctr_1")
            .with(containers::identity("db"))
            .running_at(IpAddr::V4(Ipv4Addr::new(10, 198, 2, 8)))],
    );

    assert_eq!(
        internal_dns_records(&intent(["machine_a"], "entry_test"), &[facts]),
        BTreeMap::new()
    );
}

#[test]
fn internal_dns_projection_excludes_retained_failed_and_old_revision_containers() {
    let facts = machine_facts(
        "machine_a",
        [
            containers::observation("machine_a", "ctr_failed")
                .with(containers::identity("db").entry("entry_failed"))
                .running_at(IpAddr::V4(Ipv4Addr::new(10, 198, 2, 7))),
            containers::observation("machine_a", "ctr_old")
                .with(containers::identity("db").entry("entry_old"))
                .running_at(IpAddr::V4(Ipv4Addr::new(10, 198, 2, 8))),
        ],
    );

    assert_eq!(
        internal_dns_records(&intent(["machine_a"], "entry_current"), &[facts]),
        BTreeMap::new()
    );
}

#[test]
fn internal_service_name_deserialization_rejects_invalid_names() {
    let error = serde_json::from_str::<InternalServiceName>("\"db.internal\"")
        .expect_err("missing namespace is invalid");

    assert!(error.to_string().contains("internal service name"));
}

#[test]
fn internal_service_name_from_ids_rejects_dns_labels_over_63_bytes() {
    let service_id = ServiceId::try_new("s".repeat(64)).expect("service id");
    let namespace_id = NamespaceId::try_new("default").expect("namespace id");

    assert!(InternalServiceName::try_from_ids(&service_id, &namespace_id).is_err());
}

#[test]
fn internal_service_name_from_ids_rejects_noncanonical_service_id_case() {
    let service_id = ServiceId::try_new("Database").expect("service id");
    let namespace_id = NamespaceId::try_new("default").expect("namespace id");

    assert!(InternalServiceName::try_from_ids(&service_id, &namespace_id).is_err());
}

#[test]
fn internal_service_name_from_ids_rejects_noncanonical_namespace_id_case() {
    let service_id = ServiceId::try_new("database").expect("service id");
    let namespace_id = NamespaceId::try_new("Default").expect("namespace id");

    assert!(InternalServiceName::try_from_ids(&service_id, &namespace_id).is_err());
}

#[test]
fn internal_service_name_query_parsing_canonicalizes_ascii_case() {
    let name = InternalServiceName::try_new("Database.Default.INTERNAL")
        .expect("operator query is case-insensitive");

    assert_eq!(name.as_str(), "database.default.internal");
}

fn intent<const N: usize>(machines: [&str; N], entry: &str) -> IntentSnapshot {
    IntentSnapshot {
        active_machines: machines.into_iter().map(active_machine).collect(),
        dataplane_projection: ployz_core::network::DataplaneProjection::try_new(Vec::new(), None)
            .expect("empty projection"),
        route_bindings: Vec::new(),
        serving_target_entries: vec![serving_target_entry("db", entry)],
        volume_pins: Vec::new(),
        automatic_hostname_configuration: AutomaticHostnameConfiguration::Ployz,
        ployz_dns_target: PloyzDnsTargetIntent::Enabled,
        active_certificates: Vec::new(),
    }
}

fn active_machine(id: &str) -> ActiveMachineState {
    ActiveMachineState {
        machine_id: machine_id(id),
        name: MachineName::try_new(id).expect("machine name"),
        activated_by: operation_id(&format!("op_{id}")),
        roles: InstallRolePolicy::install_all(),
        lifecycle: MachineLifecycle::Active,
        control_endpoints: Vec::new(),
        mesh_endpoints: Vec::new(),
        endpoint_subnet: MachineEndpointSubnet::try_new("10.198.0.0/24")
            .expect("valid endpoint subnet"),
        wireguard_public_key: ployz_core::network::WireGuardPublicKey::try_new(
            base64::engine::general_purpose::STANDARD.encode(Sha256::digest(id)),
        )
        .expect("public key"),
    }
}

fn machine_facts(
    machine: &str,
    observations: impl IntoIterator<Item = containers::ManagedContainerObservationBuilder>,
) -> MachineFactsSnapshot {
    MachineFactsSnapshot::try_new(
        machine_id(machine),
        containers::snapshot(machine, observations),
        None,
        fixtures::test_disk_space(),
        None,
        ployz_core::image::OciPlatform::current(),
        1,
    )
    .expect("machine facts")
}

fn cluster_id() -> ClusterId {
    ClusterId::try_new(CLUSTER).expect("cluster id")
}

fn projection_input() -> InternalDnsRowProjectionInput {
    InternalDnsRowProjectionInput {
        cluster_id: cluster_id(),
        local_machine_id: MachineRowId::try_new(LOCAL_MACHINE).expect("machine id"),
        cluster_rows: vec![stored_row(
            CLUSTER,
            serde_json::json!({
                "v": 1,
                "cluster_id": CLUSTER,
                "written_by": { "kind": "peer", "peer_id": PEER },
                "written_at": "2026-08-05T10:00:00Z",
                "name": "test",
                "storage_default": "plain",
                "hostname_mode": { "mode": "disabled" },
                "prefix": "10.210.0.0/16",
                "provider": "tailscale",
                "acme_directory_url": "https://acme.test/directory",
                "acme_contact": null
            }),
        )],
        machine_rows: vec![
            machine_row(
                LOCAL_MACHINE,
                "edge-local",
                "active",
                "tailscale",
                "10.210.20.0/24",
            ),
            machine_row(
                REMOTE_MACHINE,
                "edge-dark",
                "draining",
                "tailscale",
                "10.210.30.0/24",
            ),
            machine_row(
                WRONG_PROVIDER_MACHINE,
                "edge-wrong-provider",
                "active",
                "wireguard",
                "10.210.40.0/24",
            ),
        ],
        namespace_rows: vec![
            namespace_row(NAMESPACE, "prod"),
            namespace_row(SHADOW_NAMESPACE, "prod"),
            StoredRow::new("malformed", "{"),
        ],
        service_rows: vec![
            service_row(SERVICE, NAMESPACE, "api", ACTIVE_DEPLOY),
            service_row(SHADOW_SERVICE, NAMESPACE, "api", ACTIVE_DEPLOY),
            service_row(EMPTY_SERVICE, NAMESPACE, "worker", ACTIVE_DEPLOY),
        ],
        container_rows: vec![
            container_row(
                "container-local",
                LOCAL_MACHINE,
                SERVICE,
                NAMESPACE,
                "10.210.20.8",
                ACTIVE_DEPLOY,
            ),
            container_row(
                "container-remote-dark",
                REMOTE_MACHINE,
                SERVICE,
                NAMESPACE,
                "10.210.30.9",
                ACTIVE_DEPLOY,
            ),
            container_row(
                "container-duplicate-ip",
                LOCAL_MACHINE,
                SERVICE,
                NAMESPACE,
                "10.210.20.8",
                ACTIVE_DEPLOY,
            ),
            container_row(
                "container-old-deploy",
                LOCAL_MACHINE,
                SERVICE,
                NAMESPACE,
                "10.210.20.10",
                OLD_DEPLOY,
            ),
            container_row(
                "container-shadow-service",
                LOCAL_MACHINE,
                SHADOW_SERVICE,
                NAMESPACE,
                "10.210.20.11",
                ACTIVE_DEPLOY,
            ),
            container_row(
                "container-shadow-namespace",
                LOCAL_MACHINE,
                SERVICE,
                SHADOW_NAMESPACE,
                "10.210.20.12",
                ACTIVE_DEPLOY,
            ),
            container_row(
                "container-wrong-provider-machine",
                WRONG_PROVIDER_MACHINE,
                SERVICE,
                NAMESPACE,
                "10.210.40.9",
                ACTIVE_DEPLOY,
            ),
        ],
    }
}

fn machine_row(id: &str, name: &str, lifecycle: &str, provider: &str, subnet: &str) -> StoredRow {
    let transport = match provider {
        "tailscale" => serde_json::json!({
            "kind": "tailscale",
            "ip": "100.64.0.20",
            "subnet_v4": subnet
        }),
        "wireguard" => serde_json::json!({
            "kind": "wireguard",
            "pubkey": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "addr_v6": "fd00::40",
            "endpoint": null,
            "subnet_v4": subnet
        }),
        _ => unreachable!("fixture provider"),
    };
    stored_row(
        id,
        serde_json::json!({
            "v": 1,
            "cluster_id": CLUSTER,
            "written_by": { "kind": "peer", "peer_id": PEER },
            "written_at": "2026-08-05T10:00:00Z",
            "name": name,
            "lifecycle": lifecycle,
            "transport": transport,
            "storage": {
                "mode": "plain",
                "reason": { "kind": "default" }
            }
        }),
    )
}

fn namespace_row(id: &str, name: &str) -> StoredRow {
    stored_row(
        id,
        serde_json::json!({
            "v": 1,
            "cluster_id": CLUSTER,
            "written_by": { "kind": "peer", "peer_id": PEER },
            "written_at": "2026-08-05T10:00:00Z",
            "name": name
        }),
    )
}

fn service_row(id: &str, namespace_id: &str, name: &str, deploy: &str) -> StoredRow {
    stored_row(
        id,
        serde_json::json!({
            "v": 1,
            "cluster_id": CLUSTER,
            "written_by": { "kind": "peer", "peer_id": PEER },
            "written_at": "2026-08-05T10:00:00Z",
            "namespace_id": namespace_id,
            "name": name,
            "image": "registry.example/api@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "env_fingerprints": {},
            "mode": "replicated",
            "replicas": 1,
            "pinned_machines": [],
            "active_deploy": deploy,
            "previous_image": null,
            "deployed_at": "2026-08-05T10:00:00Z",
            "operation_id": deploy
        }),
    )
}

fn container_row(
    id: &str,
    machine_id: &str,
    service_id: &str,
    namespace_id: &str,
    ip: &str,
    deploy: &str,
) -> StoredRow {
    stored_row(
        id,
        serde_json::json!({
            "v": 1,
            "cluster_id": CLUSTER,
            "machine_id": machine_id,
            "service_id": service_id,
            "namespace_id": namespace_id,
            "ip": ip,
            "deploy": deploy
        }),
    )
}

fn stored_row(id: &str, document: serde_json::Value) -> StoredRow {
    StoredRow::new(id, serde_json::to_string(&document).expect("row document"))
}
