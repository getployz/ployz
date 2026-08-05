use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;

use ployz_core::corrosion::{
    AcmeHttp01Document, AutomaticHostnameMode, CertHoldingDocument, ClusterDocument,
    ContainerDocument, CorrosionDocument, CorrosionDocumentVersion, CorrosionExecutionFailureClass,
    CorrosionOperation, CorrosionOperationFailure, CorrosionOperationState, CorrosionTable,
    CorrosionTimestamp, IngressMode, MachineDocument, MachineLoadBand, MachineStatusDocument,
    MachineStorageIneligibleReason, MachineStorageSelection, MachineStorageSelectionReason,
    MachineTransport, MeshProvider, NameClaim, NamedCorrosionDocument, NamespaceDocument,
    OperationDocument, OperationInitiator, OperatorWriteProvenance, PeerDocument, PeerTransport,
    RouteBindingDocument, ServiceDocument, ServicePlacement, ServiceReplicaCount, Sha256Hex,
    StorageMode, TokenDocument,
};
use ployz_core::deploy::ImageReference;
use ployz_core::ids::{
    ClusterId, CorrosionUlid, MachineId, MachineRowId, NamespaceId, NamespaceRowId, OperationId,
    OperationRowId, PeerId, RouteBindingRowId, ServiceId, ServiceRowId, TokenId,
};
use ployz_core::ingress::RouteBindingOrigin;
use ployz_core::machine::{MachineLifecycle, MachineName};
use ployz_core::network::{MachineEndpointSubnet, MachineEndpointSupernet, WireGuardPublicKey};
use ployz_core::operation::{RouteHostname, RoutePort};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

const ULID_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const ULID_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

#[test]
fn corrosion_ulid_accepts_only_canonical_text_and_orders_by_value() {
    let lower = ULID_A.to_ascii_lowercase();
    let first = CorrosionUlid::try_new(ULID_A).expect("canonical ULID");
    let second = CorrosionUlid::from_str(ULID_B).expect("canonical ULID");

    assert_eq!(first.as_str(), ULID_A);
    assert!(first < second);
    for rejected in [
        lower.as_str(),
        "1ARZ3NDEKTSV4RRFFQ69G5FAV",
        "81ARZ3NDEKTSV4RRFFQ69G5FAV",
        "01ARZ3NDEKTSV4RRFFQ69G5FAI",
        "01ARZ3NDEKTSV4RRFFQ69G5FA-",
    ] {
        assert!(CorrosionUlid::try_new(rejected).is_err(), "{rejected}");
    }
}

#[test]
fn corrosion_ulid_ids_are_transparent_strings() {
    let cluster_id = ClusterId::try_new(ULID_A).expect("cluster id");
    let token_id = TokenId::try_new(ULID_A).expect("token id");
    let peer_id = PeerId::try_new(ULID_A).expect("peer id");

    assert_eq!(serde_json::to_value(cluster_id).expect("json"), ULID_A);
    assert_eq!(serde_json::to_value(token_id).expect("json"), ULID_A);
    assert_eq!(serde_json::to_value(peer_id).expect("json"), ULID_A);
}

#[test]
fn v2_row_ids_are_canonical_ulids_without_changing_incumbent_ids() {
    let row_ids = [
        MachineRowId::try_new(ULID_A)
            .expect("machine row id")
            .as_str()
            .to_owned(),
        NamespaceRowId::try_new(ULID_A)
            .expect("namespace row id")
            .as_str()
            .to_owned(),
        ServiceRowId::try_new(ULID_A)
            .expect("service row id")
            .as_str()
            .to_owned(),
        OperationRowId::try_new(ULID_A)
            .expect("operation row id")
            .as_str()
            .to_owned(),
        RouteBindingRowId::try_new(ULID_A)
            .expect("route binding row id")
            .as_str()
            .to_owned(),
    ];

    assert!(row_ids.iter().all(|id| id == ULID_A));
    assert!(MachineRowId::try_new("machine_edge_a").is_err());
    assert!(MachineId::try_new("machine_edge_a").is_ok());
    assert!(NamespaceId::try_new("namespace_production").is_ok());
    assert!(ServiceId::try_new("service_api").is_ok());
    assert!(OperationId::try_new("operation_deploy").is_ok());
}

#[test]
fn corrosion_timestamps_parse_rfc3339_order_chronologically_and_serialize_as_utc() {
    let offset =
        CorrosionTimestamp::try_new("2026-08-04T12:08:00+02:00").expect("valid offset timestamp");
    let one_nanosecond_later =
        CorrosionTimestamp::try_new("2026-08-04T10:08:00.000000001Z").expect("valid UTC timestamp");

    assert!(offset < one_nanosecond_later);
    assert_eq!(
        serde_json::to_value(offset).expect("timestamp JSON"),
        "2026-08-04T10:08:00.000000000Z"
    );
    assert!(CorrosionTimestamp::try_new("next Tuesday").is_err());
}

#[test]
fn corrosion_document_references_reject_incumbent_subject_tokens() {
    let mut document = serde_json::to_value(service_document()).expect("service JSON");
    *document
        .get_mut("namespace_id")
        .expect("service namespace reference") = json!("namespace_production");

    assert!(serde_json::from_value::<ServiceDocument>(document).is_err());
}

#[test]
fn peer_transport_rejects_container_subnets_but_tolerates_additive_fields() {
    let peer = serde_json::to_value(peer_document()).expect("peer JSON");
    assert!(
        peer.get("transport")
            .and_then(|transport| transport.get("subnet_v4"))
            .is_none()
    );

    let mut additive = peer.clone();
    additive
        .get_mut("transport")
        .and_then(Value::as_object_mut)
        .expect("peer transport object")
        .insert("future_transport_field".to_owned(), json!("tolerated"));
    serde_json::from_value::<PeerDocument>(additive).expect("additive peer transport field");

    let mut with_subnet = peer;
    with_subnet
        .get_mut("transport")
        .and_then(Value::as_object_mut)
        .expect("peer transport object")
        .insert("subnet_v4".to_owned(), json!("10.210.20.0/24"));
    assert!(serde_json::from_value::<PeerDocument>(with_subnet).is_err());
}

#[test]
fn machine_transport_requires_a_container_subnet() {
    let mut machine = serde_json::to_value(machine_document()).expect("machine JSON");
    machine
        .get_mut("transport")
        .and_then(Value::as_object_mut)
        .expect("machine transport object")
        .insert("subnet_v4".to_owned(), Value::Null);

    assert!(serde_json::from_value::<MachineDocument>(machine).is_err());
}

#[test]
fn cluster_and_machine_documents_enforce_container_prefix_shapes() {
    let mut cluster = serde_json::to_value(cluster_document()).expect("cluster JSON");
    *cluster.get_mut("prefix").expect("cluster prefix") = json!("10.210.0.0/24");
    assert!(serde_json::from_value::<ClusterDocument>(cluster).is_err());

    let mut machine = serde_json::to_value(machine_document()).expect("machine JSON");
    *machine
        .get_mut("transport")
        .and_then(|transport| transport.get_mut("subnet_v4"))
        .expect("machine subnet") = json!("10.210.20.0/25");
    assert!(serde_json::from_value::<MachineDocument>(machine).is_err());
}

#[test]
fn every_v1_document_serializes_all_public_contract_fields() {
    for fixture in document_fixtures() {
        assert_eq!(
            fixture.value.get("v").and_then(Value::as_u64),
            Some(1),
            "{} version",
            fixture.table.as_str()
        );
        assert_eq!(
            fixture.value.get("cluster_id").and_then(Value::as_str),
            Some(ULID_A),
            "{} cluster fence",
            fixture.table.as_str()
        );
        assert_eq!(fixture.document_version, CorrosionDocumentVersion::V1);
    }
}

#[test]
fn exactly_operator_authority_documents_flatten_required_write_provenance() {
    let operator_tables = BTreeSet::from([
        CorrosionTable::Cluster,
        CorrosionTable::Machines,
        CorrosionTable::Peers,
        CorrosionTable::Tokens,
        CorrosionTable::Namespaces,
        CorrosionTable::Services,
        CorrosionTable::RouteBindings,
    ]);

    for fixture in document_fixtures() {
        let written_by = fixture.value.get("written_by");
        let written_at = fixture.value.get("written_at");
        if operator_tables.contains(&fixture.table) {
            assert_eq!(
                written_by,
                Some(&json!({"kind": "peer", "peer_id": ULID_B})),
                "{} writer",
                fixture.table.as_str()
            );
            assert_eq!(
                written_at.and_then(Value::as_str),
                Some("2026-08-04T10:00:00.000000000Z"),
                "{} write time",
                fixture.table.as_str()
            );
        } else {
            assert!(written_by.is_none(), "{} writer", fixture.table.as_str());
            assert!(
                written_at.is_none(),
                "{} write time",
                fixture.table.as_str()
            );
        }
    }
}

#[test]
fn every_corrosion_document_timestamp_serializes_in_canonical_utc() {
    let fixtures = document_fixtures()
        .into_iter()
        .map(|fixture| (fixture.table, fixture.value))
        .collect::<BTreeMap<_, _>>();

    for (table, paths) in [
        (CorrosionTable::Tokens, &["created_at", "expires_at"][..]),
        (CorrosionTable::Services, &["deployed_at"]),
        (CorrosionTable::MachineStatus, &["observed_at"]),
        (CorrosionTable::Operations, &["started_at", "completed_at"]),
        (CorrosionTable::CertHoldings, &["issued_at", "expires_at"]),
        (CorrosionTable::AcmeHttp01, &["created_at"]),
    ] {
        let document = fixtures.get(&table).expect("table fixture");
        for path in paths {
            assert!(
                document
                    .get(path)
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.ends_with(".000000000Z")),
                "{}.{} canonical timestamp",
                table.as_str(),
                path
            );
        }
    }
}

#[test]
fn every_v1_document_tolerates_unknown_fields() {
    for mut fixture in document_fixtures() {
        fixture
            .value
            .as_object_mut()
            .expect("document object")
            .insert("future_addition".to_owned(), json!({"nested": true}));
        deserialize_fixture(fixture.table, fixture.value);
    }

    let mut machine = serde_json::to_value(machine_document()).expect("machine JSON");
    machine
        .get_mut("transport")
        .and_then(Value::as_object_mut)
        .expect("transport object")
        .insert("future_transport_field".to_owned(), json!("tolerated"));
    serde_json::from_value::<MachineDocument>(machine).expect("unknown nested transport field");
}

#[test]
fn generated_column_json_paths_match_typed_documents() {
    let fixtures = document_fixtures()
        .into_iter()
        .map(|fixture| (fixture.table, fixture.value))
        .collect::<BTreeMap<_, _>>();

    for (table, paths) in [
        (CorrosionTable::Machines, &["name", "lifecycle"][..]),
        (CorrosionTable::Peers, &["name"]),
        (CorrosionTable::Namespaces, &["name"]),
        (CorrosionTable::Services, &["namespace_id", "name"]),
        (
            CorrosionTable::RouteBindings,
            &["hostname", "service_id", "namespace_id"],
        ),
        (
            CorrosionTable::Containers,
            &["machine_id", "service_id", "namespace_id"],
        ),
        (CorrosionTable::Operations, &["kind", "state", "machine_id"]),
        (
            CorrosionTable::CertHoldings,
            &["hostname", "machine_id", "expires_at"],
        ),
        (CorrosionTable::AcmeHttp01, &["machine_id"]),
    ] {
        let document = fixtures.get(&table).expect("table fixture");
        for path in paths {
            assert!(
                document.get(path).is_some_and(|value| !value.is_null()),
                "{}.{} must be present and non-null",
                table.as_str(),
                path
            );
        }
    }

    assert!(
        fixtures
            .get(&CorrosionTable::Tokens)
            .expect("tokens fixture")
            .get("kind")
            .is_none_or(Value::is_null),
        "accepted v1 token documents carry no kind"
    );
}

#[test]
fn named_documents_expose_scope_aware_claims() {
    let namespace = namespace_document();
    let service = service_document();
    let route = route_binding_document();

    assert_eq!(
        namespace.name_claim(),
        NameClaim::Namespace {
            name: "production".to_owned(),
        }
    );
    assert_eq!(
        service.name_claim(),
        NameClaim::Service {
            namespace_id: namespace_id(),
            name: "api".to_owned(),
        }
    );
    assert_eq!(
        route.name_claim(),
        NameClaim::RouteBinding {
            hostname: RouteHostname::try_new("api.example.com").expect("hostname"),
        }
    );
}

#[test]
fn every_corrosion_operation_variant_roundtrips_without_flattened_key_collisions() {
    let variants = [
        CorrosionOperation::Build {
            service_id: service_id(),
        },
        CorrosionOperation::Deploy {
            namespace_id: namespace_id(),
            service_id: service_id(),
        },
        CorrosionOperation::MachineAdd {
            target_machine_id: machine_id(),
        },
        CorrosionOperation::MachineRemove {
            target_machine_id: machine_id(),
        },
        CorrosionOperation::Recovery {
            target_machine_id: machine_id(),
        },
    ];

    for operation in variants {
        let document = OperationDocument {
            operation,
            ..operation_document()
        };
        let encoded = serde_json::to_string(&document).expect("operation document serializes");
        let decoded = serde_json::from_str::<OperationDocument>(&encoded)
            .expect("operation document round-trips");
        assert_eq!(decoded, document);
    }
}

struct DocumentFixture {
    table: CorrosionTable,
    document_version: CorrosionDocumentVersion,
    value: Value,
}

fn fixture<T>(document: T) -> DocumentFixture
where
    T: CorrosionDocument,
{
    DocumentFixture {
        table: T::TABLE,
        document_version: document.version(),
        value: serde_json::to_value(document).expect("document JSON"),
    }
}

fn document_fixtures() -> Vec<DocumentFixture> {
    vec![
        fixture(cluster_document()),
        fixture(machine_document()),
        fixture(peer_document()),
        fixture(token_document()),
        fixture(namespace_document()),
        fixture(service_document()),
        fixture(route_binding_document()),
        fixture(container_document()),
        fixture(machine_status_document()),
        fixture(operation_document()),
        fixture(cert_holding_document()),
        fixture(acme_http01_document()),
    ]
}

fn deserialize_fixture(table: CorrosionTable, value: Value) {
    match table {
        CorrosionTable::Cluster => deserialize::<ClusterDocument>(value),
        CorrosionTable::Machines => deserialize::<MachineDocument>(value),
        CorrosionTable::Peers => deserialize::<PeerDocument>(value),
        CorrosionTable::Tokens => deserialize::<TokenDocument>(value),
        CorrosionTable::Namespaces => deserialize::<NamespaceDocument>(value),
        CorrosionTable::Services => deserialize::<ServiceDocument>(value),
        CorrosionTable::RouteBindings => deserialize::<RouteBindingDocument>(value),
        CorrosionTable::Containers => deserialize::<ContainerDocument>(value),
        CorrosionTable::MachineStatus => deserialize::<MachineStatusDocument>(value),
        CorrosionTable::Operations => deserialize::<OperationDocument>(value),
        CorrosionTable::CertHoldings => deserialize::<CertHoldingDocument>(value),
        CorrosionTable::AcmeHttp01 => deserialize::<AcmeHttp01Document>(value),
    }
}

fn deserialize<T: DeserializeOwned>(value: Value) {
    serde_json::from_value::<T>(value).expect("document accepts additive fields");
}

fn version() -> CorrosionDocumentVersion {
    CorrosionDocumentVersion::V1
}

fn timestamp(value: &str) -> CorrosionTimestamp {
    CorrosionTimestamp::try_new(value).expect("timestamp")
}

fn operator_provenance() -> OperatorWriteProvenance {
    OperatorWriteProvenance {
        written_by: OperationInitiator::Peer {
            peer_id: PeerId::try_new(ULID_B).expect("peer id"),
        },
        written_at: timestamp("2026-08-04T10:00:00Z"),
    }
}

fn cluster_id() -> ClusterId {
    ClusterId::try_new(ULID_A).expect("cluster id")
}

fn machine_id() -> MachineRowId {
    MachineRowId::try_new(ULID_B).expect("machine row id")
}

fn namespace_id() -> NamespaceRowId {
    NamespaceRowId::try_new(ULID_B).expect("namespace row id")
}

fn service_id() -> ServiceRowId {
    ServiceRowId::try_new(ULID_B).expect("service row id")
}

fn operation_id() -> OperationRowId {
    OperationRowId::try_new(ULID_B).expect("operation row id")
}

fn cluster_document() -> ClusterDocument {
    ClusterDocument {
        v: version(),
        cluster_id: cluster_id(),
        provenance: operator_provenance(),
        name: "acme-prod".to_owned(),
        storage_default: StorageMode::Plain,
        hostname_mode: AutomaticHostnameMode::Custom {
            suffix: RouteHostname::try_new("apps.example.com").expect("suffix"),
        },
        prefix: MachineEndpointSupernet::try_new("10.210.0.0/16").expect("prefix"),
        provider: MeshProvider::BuiltinWireguard,
        acme_directory_url: "https://acme.example/directory".to_owned(),
        acme_contact: Some("mailto:ops@example.com".to_owned()),
    }
}

fn machine_document() -> MachineDocument {
    MachineDocument {
        v: version(),
        cluster_id: cluster_id(),
        provenance: operator_provenance(),
        name: MachineName::try_new("edge-a").expect("machine name"),
        lifecycle: MachineLifecycle::Active,
        transport: MachineTransport::Wireguard {
            pubkey: WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                .expect("public key"),
            addr_v6: Ipv6Addr::from_str("fd00::20").expect("IPv6"),
            endpoint: Some(SocketAddr::from_str("192.0.2.10:51820").expect("endpoint")),
            subnet_v4: MachineEndpointSubnet::try_new("10.210.20.0/24").expect("subnet"),
        },
        storage: MachineStorageSelection {
            mode: StorageMode::Plain,
            reason: MachineStorageSelectionReason::Ineligible {
                reason: MachineStorageIneligibleReason::LowRam,
            },
        },
    }
}

fn peer_document() -> PeerDocument {
    PeerDocument {
        v: version(),
        cluster_id: cluster_id(),
        provenance: operator_provenance(),
        name: "operator-laptop".to_owned(),
        transport: PeerTransport::Tailscale {
            ip: Ipv4Addr::new(100, 64, 0, 10),
        },
    }
}

fn token_document() -> TokenDocument {
    TokenDocument {
        v: version(),
        cluster_id: cluster_id(),
        provenance: operator_provenance(),
        secret_sha256: Sha256Hex::try_new("a".repeat(64)).expect("digest"),
        created_at: timestamp("2026-08-04T10:00:00Z"),
        expires_at: timestamp("2026-08-05T10:00:00Z"),
    }
}

fn namespace_document() -> NamespaceDocument {
    NamespaceDocument {
        v: version(),
        cluster_id: cluster_id(),
        provenance: operator_provenance(),
        name: "production".to_owned(),
    }
}

fn service_document() -> ServiceDocument {
    ServiceDocument {
        v: version(),
        cluster_id: cluster_id(),
        provenance: operator_provenance(),
        namespace_id: namespace_id(),
        name: "api".to_owned(),
        image: ImageReference::try_new("ghcr.io/acme/api:2026-08-04").expect("image"),
        env_fingerprints: BTreeMap::from([(
            "DATABASE_URL".to_owned(),
            Sha256Hex::try_new("b".repeat(64)).expect("digest"),
        )]),
        placement: ServicePlacement::Replicated {
            replicas: ServiceReplicaCount::try_new(3).expect("replicas"),
        },
        pinned_machines: BTreeSet::from([machine_id()]),
        active_deploy: operation_id(),
        previous_image: Some(
            ImageReference::try_new("ghcr.io/acme/api:previous").expect("previous image"),
        ),
        deployed_at: timestamp("2026-08-04T10:05:00Z"),
        operation_id: operation_id(),
    }
}

fn route_binding_document() -> RouteBindingDocument {
    RouteBindingDocument {
        v: version(),
        cluster_id: cluster_id(),
        provenance: operator_provenance(),
        hostname: RouteHostname::try_new("api.example.com").expect("hostname"),
        service_id: service_id(),
        namespace_id: namespace_id(),
        endpoint_port: RoutePort::try_new(8080).expect("port"),
        origin: RouteBindingOrigin::Declared,
        ingress_mode: IngressMode::Direct,
    }
}

fn container_document() -> ContainerDocument {
    ContainerDocument {
        v: version(),
        cluster_id: cluster_id(),
        machine_id: machine_id(),
        service_id: service_id(),
        namespace_id: namespace_id(),
        ip: Ipv4Addr::new(10, 210, 20, 2),
        deploy: operation_id(),
    }
}

fn machine_status_document() -> MachineStatusDocument {
    MachineStatusDocument {
        v: version(),
        cluster_id: cluster_id(),
        machine_id: machine_id(),
        ployz_version: "0.1.0-alpha.7".to_owned(),
        corrosion_version: "0.2.0-beta.0".to_owned(),
        architecture: "x86_64".to_owned(),
        free_disk_bytes: 80_000_000_000,
        free_memory_bytes: 4_000_000_000,
        load: MachineLoadBand::Idle,
        observed_at: timestamp("2026-08-04T10:06:00Z"),
        mesh: None,
        container_isolation: None,
    }
}

fn operation_document() -> OperationDocument {
    OperationDocument {
        v: version(),
        cluster_id: cluster_id(),
        machine_id: machine_id(),
        operation: CorrosionOperation::Deploy {
            namespace_id: namespace_id(),
            service_id: service_id(),
        },
        initiator: OperationInitiator::Peer {
            peer_id: PeerId::try_new(ULID_B).expect("peer id"),
        },
        status: CorrosionOperationState::Failed {
            started_at: timestamp("2026-08-04T10:00:00Z"),
            completed_at: timestamp("2026-08-04T10:07:00Z"),
            failure: CorrosionOperationFailure::Execution {
                class: CorrosionExecutionFailureClass::HealthGateFailed,
                message: "container did not become healthy".to_owned(),
            },
        },
    }
}

fn cert_holding_document() -> CertHoldingDocument {
    CertHoldingDocument {
        v: version(),
        cluster_id: cluster_id(),
        machine_id: machine_id(),
        hostname: RouteHostname::try_new("api.example.com").expect("hostname"),
        fingerprint: Sha256Hex::try_new("c".repeat(64)).expect("fingerprint"),
        issued_at: timestamp("2026-08-04T10:08:00Z"),
        expires_at: timestamp("2026-11-02T10:08:00Z"),
    }
}

fn acme_http01_document() -> AcmeHttp01Document {
    AcmeHttp01Document {
        v: version(),
        cluster_id: cluster_id(),
        machine_id: machine_id(),
        hostname: RouteHostname::try_new("api.example.com").expect("hostname"),
        key_authorization: "public-acme-key-authorization".to_owned(),
        created_at: timestamp("2026-08-04T10:09:00Z"),
    }
}
