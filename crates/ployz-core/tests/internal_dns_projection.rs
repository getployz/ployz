use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};

use ployz_core::corrosion::StoredRow;
use ployz_core::ids::{ClusterName, MachineName};
use ployz_core::network::internal_dns::{
    INTERNAL_DNS_READINESS_NAME, InternalDnsRowProjectionError, InternalDnsRowProjectionInput,
    InternalDnsSearchDomain, InternalServiceName, project_internal_dns_rows,
};

const CLUSTER: &str = "main";
const LOCAL_MACHINE: &str = "edge-local";
const REMOTE_MACHINE: &str = "edge-dark";
const WRONG_PROVIDER_MACHINE: &str = "edge-wrong-provider";
const NAMESPACE: &str = "prod";
const SERVICE: &str = "api";
const ACTIVE_DEPLOY: &str = "blue";
const OLD_DEPLOY: &str = "green";
const PEER: &str = "operator";
const EMPTY_SERVICE: &str = "worker";

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
    const READINESS_NAMESPACE: &str = "ployz";
    const READINESS_SERVICE: &str = "readiness";
    input.namespace_rows.push(namespace_row(
        READINESS_NAMESPACE,
        "ployz",
        &[(READINESS_SERVICE, ACTIVE_DEPLOY)],
    ));
    let local = input
        .machine_endpoint_rows
        .iter_mut()
        .find(|row| row.key == LOCAL_MACHINE)
        .expect("local endpoint testimony");
    let mut document: serde_json::Value =
        serde_json::from_str(&local.document).expect("endpoint document");
    document
        .get_mut("endpoints")
        .expect("endpoints field")
        .as_array_mut()
        .expect("endpoint list")
        .push(endpoint(
            READINESS_SERVICE,
            READINESS_NAMESPACE,
            "10.210.20.53",
            ACTIVE_DEPLOY,
            "global",
        ));
    local.document = serde_json::to_string(&document).expect("endpoint document");

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
        MachineName::try_new("missing-machine").expect("machine name");
    assert_eq!(
        project_internal_dns_rows(missing_machine),
        Err(InternalDnsRowProjectionError::LocalMachineMissing {
            machine_id: MachineName::try_new("missing-machine").expect("machine name"),
        })
    );
}

#[test]
fn corrosion_row_projection_retains_a_dark_draining_machine_without_liveness_testimony() {
    let mut input = projection_input();
    input.namespace_rows = vec![namespace_row(
        NAMESPACE,
        NAMESPACE,
        &[(SERVICE, ACTIVE_DEPLOY)],
    )];
    input.machine_endpoint_rows = vec![
        machine_endpoint_row(
            REMOTE_MACHINE,
            vec![endpoint(
                SERVICE,
                NAMESPACE,
                "10.210.30.9",
                ACTIVE_DEPLOY,
                "global",
            )],
        ),
        machine_endpoint_row(
            LOCAL_MACHINE,
            vec![endpoint(
                SERVICE,
                NAMESPACE,
                "10.210.20.10",
                OLD_DEPLOY,
                "global",
            )],
        ),
    ];

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

fn cluster_id() -> ClusterName {
    ClusterName::try_new(CLUSTER).expect("cluster id")
}

fn projection_input() -> InternalDnsRowProjectionInput {
    InternalDnsRowProjectionInput {
        cluster_id: cluster_id(),
        local_machine_id: MachineName::try_new(LOCAL_MACHINE).expect("machine id"),
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
                LOCAL_MACHINE,
                "active",
                "tailscale",
                "10.210.20.0/24",
            ),
            machine_row(
                REMOTE_MACHINE,
                REMOTE_MACHINE,
                "draining",
                "tailscale",
                "10.210.30.0/24",
            ),
            machine_row(
                WRONG_PROVIDER_MACHINE,
                WRONG_PROVIDER_MACHINE,
                "active",
                "wireguard",
                "10.210.40.0/24",
            ),
        ],
        namespace_rows: vec![
            namespace_row(
                NAMESPACE,
                NAMESPACE,
                &[(SERVICE, ACTIVE_DEPLOY), (EMPTY_SERVICE, ACTIVE_DEPLOY)],
            ),
            StoredRow::new("malformed", "{"),
        ],
        machine_endpoint_rows: vec![
            machine_endpoint_row(
                LOCAL_MACHINE,
                vec![
                    endpoint(SERVICE, NAMESPACE, "10.210.20.8", ACTIVE_DEPLOY, "global"),
                    endpoint(SERVICE, NAMESPACE, "10.210.20.8", ACTIVE_DEPLOY, "1"),
                    endpoint(SERVICE, NAMESPACE, "10.210.20.10", OLD_DEPLOY, "global"),
                ],
            ),
            machine_endpoint_row(
                REMOTE_MACHINE,
                vec![endpoint(
                    SERVICE,
                    NAMESPACE,
                    "10.210.30.9",
                    ACTIVE_DEPLOY,
                    "global",
                )],
            ),
            machine_endpoint_row(
                WRONG_PROVIDER_MACHINE,
                vec![endpoint(
                    SERVICE,
                    NAMESPACE,
                    "10.210.40.9",
                    ACTIVE_DEPLOY,
                    "global",
                )],
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

fn namespace_row(id: &str, name: &str, services: &[(&str, &str)]) -> StoredRow {
    let services = services
        .iter()
        .map(|(service, deploy)| ((*service).to_owned(), published_service(deploy)))
        .collect::<serde_json::Map<_, _>>();
    stored_row(
        id,
        serde_json::json!({
            "v": 1,
            "cluster_id": CLUSTER,
            "written_by": { "kind": "peer", "peer_id": PEER },
            "written_at": "2026-08-05T10:00:00Z",
            "name": name,
            "services": services,
        }),
    )
}

fn published_service(deploy: &str) -> serde_json::Value {
    serde_json::json!({
        "image": "registry.example/api@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "env_fingerprints": {},
        "mode": "replicated",
        "replicas": 1,
        "pinned_machines": [],
        "active_deploy": deploy,
        "previous_image": null,
        "deployed_at": "2026-08-05T10:00:00Z",
    })
}

fn machine_endpoint_row(machine_id: &str, endpoints: Vec<serde_json::Value>) -> StoredRow {
    stored_row(
        machine_id,
        serde_json::json!({
            "v": 1,
            "cluster_id": CLUSTER,
            "machine_id": machine_id,
            "observed_at": "2026-08-05T10:00:00Z",
            "endpoints": endpoints,
        }),
    )
}

fn endpoint(
    service_name: &str,
    namespace_id: &str,
    ip: &str,
    deploy: &str,
    replica_slot: &str,
) -> serde_json::Value {
    let replica_slot = if replica_slot == "global" {
        serde_json::json!({ "kind": "global" })
    } else {
        serde_json::json!({
            "kind": "replicated",
            "number": replica_slot.parse::<u16>().expect("replica slot")
        })
    };
    serde_json::json!({
        "namespace_id": namespace_id,
        "service_name": service_name,
        "replica_slot": replica_slot,
        "ip": ip,
        "deploy": deploy
    })
}

fn stored_row(id: &str, document: serde_json::Value) -> StoredRow {
    StoredRow::new(id, serde_json::to_string(&document).expect("row document"))
}
