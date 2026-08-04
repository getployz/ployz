use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;

use ipnet::Ipv4Net;
use ployz_core::corrosion::{
    AcmeHttp01Document, AutomaticHostnameMode, CertHoldingDocument, ClusterDocument,
    ContainerDocument, CorrosionDocument, CorrosionDocumentVersion, CorrosionExecutionFailureClass,
    CorrosionOperation, CorrosionOperationFailure, CorrosionOperationState, CorrosionTable,
    IngressMode, MachineDocument, MachineLoadBand, MachineStatusDocument,
    MachineStorageIneligibleReason, MachineStorageSelection, MachineStorageSelectionReason,
    MeshProvider, NameClaim, NamedCorrosionDocument, NamespaceDocument, OperationDocument,
    OperationInitiator, PeerDocument, RouteBindingDocument, ServiceDocument, ServicePlacement,
    ServiceReplicaCount, Sha256Hex, StorageMode, TokenDocument, Transport,
};
use ployz_core::deploy::ImageReference;
use ployz_core::ids::{
    ClusterId, CorrosionUlid, MachineId, NamespaceId, OperationId, PeerId, ServiceId, TokenId,
};
use ployz_core::ingress::RouteBindingOrigin;
use ployz_core::machine::{MachineLifecycle, MachineName};
use ployz_core::network::WireGuardPublicKey;
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

fn cluster_id() -> ClusterId {
    ClusterId::try_new(ULID_A).expect("cluster id")
}

fn machine_id() -> MachineId {
    MachineId::try_new(ULID_B).expect("machine id")
}

fn namespace_id() -> NamespaceId {
    NamespaceId::try_new("namespace_production").expect("namespace id")
}

fn service_id() -> ServiceId {
    ServiceId::try_new("service_api").expect("service id")
}

fn operation_id() -> OperationId {
    OperationId::try_new("operation_deploy").expect("operation id")
}

fn cluster_document() -> ClusterDocument {
    ClusterDocument {
        v: version(),
        cluster_id: cluster_id(),
        name: "acme-prod".to_owned(),
        storage_default: StorageMode::Plain,
        hostname_mode: AutomaticHostnameMode::Custom {
            suffix: RouteHostname::try_new("apps.example.com").expect("suffix"),
        },
        prefix: Ipv4Net::from_str("10.210.0.0/16").expect("prefix"),
        provider: MeshProvider::BuiltinWireguard,
        acme_directory_url: "https://acme.example/directory".to_owned(),
        acme_contact: Some("mailto:ops@example.com".to_owned()),
    }
}

fn machine_document() -> MachineDocument {
    MachineDocument {
        v: version(),
        cluster_id: cluster_id(),
        name: MachineName::try_new("edge-a").expect("machine name"),
        lifecycle: MachineLifecycle::Active,
        transport: Transport::Wireguard {
            pubkey: WireGuardPublicKey::try_new("wireguard-public-key").expect("public key"),
            addr_v6: Ipv6Addr::from_str("fd00::20").expect("IPv6"),
            endpoint: Some(SocketAddr::from_str("192.0.2.10:51820").expect("endpoint")),
            subnet_v4: Some(Ipv4Net::from_str("10.210.20.0/24").expect("subnet")),
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
        name: "operator-laptop".to_owned(),
        transport: Transport::Tailscale {
            ip: Ipv4Addr::new(100, 64, 0, 10),
            subnet_v4: None,
        },
    }
}

fn token_document() -> TokenDocument {
    TokenDocument {
        v: version(),
        cluster_id: cluster_id(),
        secret_sha256: Sha256Hex::try_new("a".repeat(64)).expect("digest"),
        created_at: "2026-08-04T10:00:00Z".to_owned(),
        expires_at: "2026-08-05T10:00:00Z".to_owned(),
    }
}

fn namespace_document() -> NamespaceDocument {
    NamespaceDocument {
        v: version(),
        cluster_id: cluster_id(),
        name: "production".to_owned(),
    }
}

fn service_document() -> ServiceDocument {
    ServiceDocument {
        v: version(),
        cluster_id: cluster_id(),
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
        deployed_at: "2026-08-04T10:05:00Z".to_owned(),
        operation_id: operation_id(),
    }
}

fn route_binding_document() -> RouteBindingDocument {
    RouteBindingDocument {
        v: version(),
        cluster_id: cluster_id(),
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
        observed_at: "2026-08-04T10:06:00Z".to_owned(),
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
            started_at: "2026-08-04T10:00:00Z".to_owned(),
            completed_at: "2026-08-04T10:07:00Z".to_owned(),
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
        issued_at: "2026-08-04T10:08:00Z".to_owned(),
        expires_at: "2026-11-02T10:08:00Z".to_owned(),
    }
}

fn acme_http01_document() -> AcmeHttp01Document {
    AcmeHttp01Document {
        v: version(),
        cluster_id: cluster_id(),
        machine_id: machine_id(),
        hostname: RouteHostname::try_new("api.example.com").expect("hostname"),
        key_authorization: "public-acme-key-authorization".to_owned(),
        created_at: "2026-08-04T10:09:00Z".to_owned(),
    }
}
