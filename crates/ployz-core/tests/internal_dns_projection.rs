use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};

use ployz_core::corrosion::StoredRow;
use ployz_core::ids::{ClusterId, MachineRowId};
use ployz_core::network::internal_dns::{
    INTERNAL_DNS_READINESS_NAME, InternalDnsRowProjectionError, InternalDnsRowProjectionInput,
    InternalDnsSearchDomain, InternalServiceName, project_internal_dns_rows,
};

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
fn internal_service_name_deserialization_rejects_invalid_names() {
    let error = serde_json::from_str::<InternalServiceName>("\"db.internal\"")
        .expect_err("missing namespace is invalid");

    assert!(error.to_string().contains("internal service name"));
}

#[test]
fn internal_service_name_query_parsing_canonicalizes_ascii_case() {
    let name = InternalServiceName::try_new("Database.Default.INTERNAL")
        .expect("operator query is case-insensitive");

    assert_eq!(name.as_str(), "database.default.internal");
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
