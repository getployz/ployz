use ployz_core::corrosion::{
    AutomaticHostnameMode, ClusterDocument, CorrosionDocumentVersion, CorrosionTimestamp,
    MeshProvider, OperatorWriteProvenance, Principal, StorageMode, StoredRow,
};
use ployz_core::ids::{ClusterId, NamespaceRowId, PeerId, RouteBindingRowId, ServiceRowId};
use ployz_core::network::MachineEndpointSupernet;
use ployz_core::operation::RouteHostname;
use ployz_core::{
    KnownApiFeature, NAMESPACE_REMOVE_ROW_ROUTE, NamespaceRemoveRowRefusal,
    NamespaceRemoveRowRequest, NamespaceRemoveRowSelection, PEER_REMOVE_ROUTE, PeerRemoveRefusal,
    PeerRemoveRequest, PeerRemoveSelection, ROUTE_REMOVE_ROUTE, RouteRemoveRefusal,
    RouteRemoveRequest, RouteRemoveSelection, SERVICE_REMOVE_ROW_ROUTE, ServiceRemoveRowRefusal,
    ServiceRemoveRowRequest, ServiceRemoveRowSelection, V2Method, V2Route,
    select_namespace_removal, select_peer_removal, select_route_removal, select_service_removal,
};
use serde_json::{Value, json};

const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const ID_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const ID_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const ID_C: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";

fn cluster() -> ClusterDocument {
    ClusterDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: ClusterId::try_new(CLUSTER).expect("cluster id"),
        provenance: OperatorWriteProvenance {
            written_by: Principal::Peer {
                peer_id: PeerId::try_new(ID_C).expect("peer id"),
            },
            written_at: CorrosionTimestamp::try_new("2026-08-05T10:00:00Z").expect("timestamp"),
        },
        name: "acme".to_owned(),
        storage_default: StorageMode::Plain,
        hostname_mode: AutomaticHostnameMode::Disabled,
        prefix: MachineEndpointSupernet::try_new("10.210.0.0/16").expect("supernet"),
        provider: MeshProvider::BuiltinWireguard,
        acme_directory_url: "https://acme.example/directory".to_owned(),
        acme_contact: None,
    }
}

fn row(id: &str, document: Value) -> StoredRow {
    StoredRow::new(id, serde_json::to_string(&document).expect("document JSON"))
}

fn named_document(table: &str, name: &str) -> Value {
    let base = json!({
        "v": 1,
        "cluster_id": CLUSTER,
        "written_by": {"kind": "peer", "peer_id": ID_C},
        "written_at": "2026-08-05T10:00:00.000000000Z"
    });
    let mut fields = base.as_object().expect("object").clone();
    match table {
        "peers" => {
            fields.insert("name".to_owned(), json!(name));
            fields.insert(
                "transport".to_owned(),
                json!({"kind":"wireguard","pubkey":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","addr_v6":"fd00::30","endpoint":null}),
            );
        }
        "namespaces" => {
            fields.insert("name".to_owned(), json!(name));
        }
        "services" => {
            fields.extend(
                json!({
                    "namespace_id": CLUSTER, "name": name,
                    "image": "ghcr.io/acme/web:latest", "env_fingerprints": {},
                    "mode": "global", "pinned_machines": [], "active_deploy": CLUSTER,
                    "previous_image": null, "deployed_at": "2026-08-05T10:00:00.000000000Z",
                    "operation_id": CLUSTER
                })
                .as_object()
                .expect("object")
                .clone(),
            );
        }
        "route_bindings" => {
            fields.extend(
                json!({
                    "hostname": name, "service_id": CLUSTER, "namespace_id": CLUSTER,
                    "endpoint_port": 8080, "origin": "declared", "ingress_mode": "direct"
                })
                .as_object()
                .expect("object")
                .clone(),
            );
        }
        _ => panic!("unknown table"),
    }
    Value::Object(fields)
}

fn service_document(namespace_id: &str, name: &str) -> Value {
    let mut document = named_document("services", name);
    *document
        .get_mut("namespace_id")
        .expect("service fixture has namespace identity") = json!(namespace_id);
    document
}

#[test]
fn every_named_removal_route_is_post_peer_only_and_distinctly_advertised() {
    let principal = Principal::Peer {
        peer_id: PeerId::try_new(ID_C).expect("peer id"),
    };
    for (path, route, feature) in [
        (
            PEER_REMOVE_ROUTE,
            V2Route::PeerRemove,
            KnownApiFeature::PeerRemove,
        ),
        (
            NAMESPACE_REMOVE_ROW_ROUTE,
            V2Route::NamespaceRemove,
            KnownApiFeature::NamespaceRemove,
        ),
        (
            SERVICE_REMOVE_ROW_ROUTE,
            V2Route::ServiceRemove,
            KnownApiFeature::ServiceRemove,
        ),
        (
            ROUTE_REMOVE_ROUTE,
            V2Route::RouteRemove,
            KnownApiFeature::RouteRemove,
        ),
    ] {
        assert_eq!(V2Route::parse(path), Some(route.clone()));
        assert_eq!(route.path(), path);
        assert_eq!(route.method(), V2Method::Post);
        assert_eq!(route.feature(), feature);
        assert!(route.accepts_principal(&principal));
    }
}

#[test]
fn peer_removal_includes_valid_shadows_and_fences_provider_mismatch() {
    let cluster = cluster();
    let request = PeerRemoveRequest {
        name: "operator".to_owned(),
        peer_id: None,
    };
    let mut mismatched = named_document("peers", "operator");
    *mismatched
        .get_mut("transport")
        .expect("peer fixture has transport") = json!({"kind": "tailscale", "ip": "100.64.0.30"});
    assert_eq!(
        select_peer_removal(
            &cluster,
            vec![
                row(ID_B, named_document("peers", "operator")),
                row(ID_A, named_document("peers", "operator")),
                row(ID_C, mismatched),
            ],
            &request,
        ),
        Err(PeerRemoveRefusal::Ambiguous {
            name: "operator".to_owned(),
            peer_ids: vec![
                PeerId::try_new(ID_A).expect("id"),
                PeerId::try_new(ID_B).expect("id"),
            ],
        })
    );
}

#[test]
fn qualified_removal_distinguishes_absence_mismatch_and_unselectable_storage() {
    let cluster = cluster();
    let selected = select_peer_removal(
        &cluster,
        vec![row(ID_A, named_document("peers", "other"))],
        &PeerRemoveRequest {
            name: "operator".to_owned(),
            peer_id: Some(PeerId::try_new(ID_A).expect("id")),
        },
    );
    assert_eq!(
        selected,
        Err(PeerRemoveRefusal::NameMismatch {
            peer_id: PeerId::try_new(ID_A).expect("id"),
            requested: "operator".to_owned(),
            found: "other".to_owned(),
        })
    );

    let skipped = row(
        ID_A,
        json!({"v": 2, "cluster_id": CLUSTER, "name": "operator"}),
    );
    assert_eq!(
        select_peer_removal(
            &cluster,
            vec![skipped],
            &PeerRemoveRequest {
                name: "operator".to_owned(),
                peer_id: Some(PeerId::try_new(ID_A).expect("id")),
            },
        ),
        Err(PeerRemoveRefusal::StoredRowUnselectable {
            peer_id: PeerId::try_new(ID_A).expect("id"),
        })
    );

    assert_eq!(
        select_peer_removal(
            &cluster,
            Vec::new(),
            &PeerRemoveRequest {
                name: "operator".to_owned(),
                peer_id: Some(PeerId::try_new(ID_A).expect("id")),
            },
        ),
        Ok(PeerRemoveSelection::AlreadyAbsent {
            peer_id: PeerId::try_new(ID_A).expect("id"),
        })
    );
}

#[test]
fn each_ordinary_named_table_selects_exactly_one_valid_row() {
    let cluster_id = ClusterId::try_new(CLUSTER).expect("cluster id");
    assert!(matches!(
        select_namespace_removal(
            &cluster_id,
            vec![row(ID_A, named_document("namespaces", "prod"))],
            &NamespaceRemoveRowRequest { name: "prod".to_owned(), namespace_id: None },
        ),
        Ok(NamespaceRemoveRowSelection::Delete { namespace_id, .. }) if namespace_id.as_str() == ID_A
    ));
    assert!(matches!(
        select_service_removal(
            &cluster_id,
            vec![row(ID_B, named_document("services", "web"))],
            &ServiceRemoveRowRequest {
                namespace_id: NamespaceRowId::try_new(CLUSTER).expect("namespace id"),
                name: "web".to_owned(),
                service_id: None,
            },
        ),
        Ok(ServiceRemoveRowSelection::Delete { service_id, .. }) if service_id.as_str() == ID_B
    ));
    assert!(matches!(
        select_route_removal(
            &cluster_id,
            vec![row(ID_C, named_document("route_bindings", "web.example.com"))],
            &RouteRemoveRequest {
                hostname: RouteHostname::try_new("web.example.com").expect("hostname"),
                route_id: None,
            },
        ),
        Ok(RouteRemoveSelection::Delete { route_id, .. }) if route_id.as_str() == ID_C
    ));
}

#[test]
fn ordinary_shadow_ids_are_sorted_and_an_unqualified_missing_name_refuses() {
    let cluster_id = ClusterId::try_new(CLUSTER).expect("cluster id");
    assert_eq!(
        select_namespace_removal(
            &cluster_id,
            vec![
                row(ID_B, named_document("namespaces", "prod")),
                row(ID_A, named_document("namespaces", "prod")),
            ],
            &NamespaceRemoveRowRequest {
                name: "prod".to_owned(),
                namespace_id: None
            },
        ),
        Err(NamespaceRemoveRowRefusal::Ambiguous {
            name: "prod".to_owned(),
            namespace_ids: vec![
                NamespaceRowId::try_new(ID_A).expect("id"),
                NamespaceRowId::try_new(ID_B).expect("id"),
            ],
        })
    );
    assert_eq!(
        select_namespace_removal(
            &cluster_id,
            Vec::new(),
            &NamespaceRemoveRowRequest {
                name: "prod".to_owned(),
                namespace_id: None
            },
        ),
        Err(NamespaceRemoveRowRefusal::NotFound {
            name: "prod".to_owned()
        })
    );

    let _: ServiceRowId = ServiceRowId::try_new(ID_A).expect("service id");
    let _: RouteBindingRowId = RouteBindingRowId::try_new(ID_A).expect("route id");
}

#[test]
fn every_ordinary_table_selects_a_valid_shadow_by_id_and_fences_raw_id_retries() {
    let cluster_id = ClusterId::try_new(CLUSTER).expect("cluster id");
    let namespace_id = NamespaceRowId::try_new(ID_B).expect("namespace id");
    assert!(matches!(
        select_namespace_removal(
            &cluster_id,
            vec![
                row(ID_A, named_document("namespaces", "prod")),
                row(ID_B, named_document("namespaces", "prod")),
            ],
            &NamespaceRemoveRowRequest {
                name: "prod".to_owned(),
                namespace_id: Some(namespace_id),
            },
        ),
        Ok(NamespaceRemoveRowSelection::Delete { namespace_id, .. }) if namespace_id.as_str() == ID_B
    ));
    assert_eq!(
        select_namespace_removal(
            &cluster_id,
            vec![row(
                ID_B,
                json!({"v": 2, "cluster_id": CLUSTER, "name": "prod"})
            )],
            &NamespaceRemoveRowRequest {
                name: "prod".to_owned(),
                namespace_id: Some(NamespaceRowId::try_new(ID_B).expect("id")),
            },
        ),
        Err(NamespaceRemoveRowRefusal::StoredRowUnselectable {
            namespace_id: NamespaceRowId::try_new(ID_B).expect("id"),
        })
    );

    let service_request = ServiceRemoveRowRequest {
        namespace_id: NamespaceRowId::try_new(CLUSTER).expect("namespace id"),
        name: "web".to_owned(),
        service_id: None,
    };
    assert_eq!(
        select_service_removal(
            &cluster_id,
            vec![
                row(ID_B, named_document("services", "web")),
                row(ID_A, named_document("services", "web")),
            ],
            &service_request,
        ),
        Err(ServiceRemoveRowRefusal::Ambiguous {
            namespace_id: NamespaceRowId::try_new(CLUSTER).expect("namespace id"),
            name: "web".to_owned(),
            service_ids: vec![
                ServiceRowId::try_new(ID_A).expect("id"),
                ServiceRowId::try_new(ID_B).expect("id"),
            ],
        })
    );
    assert_eq!(
        select_service_removal(
            &cluster_id,
            vec![row(ID_A, named_document("services", "worker"))],
            &ServiceRemoveRowRequest {
                namespace_id: NamespaceRowId::try_new(CLUSTER).expect("namespace id"),
                name: "web".to_owned(),
                service_id: Some(ServiceRowId::try_new(ID_A).expect("id")),
            },
        ),
        Err(ServiceRemoveRowRefusal::IdentityMismatch {
            service_id: ServiceRowId::try_new(ID_A).expect("id"),
            requested_namespace_id: NamespaceRowId::try_new(CLUSTER).expect("namespace id"),
            requested_name: "web".to_owned(),
            found_namespace_id: NamespaceRowId::try_new(CLUSTER).expect("namespace id"),
            found_name: "worker".to_owned(),
        })
    );
    assert_eq!(
        select_service_removal(
            &cluster_id,
            vec![row(
                ID_A,
                json!({"v": 1, "cluster_id": "01ARZ3NDEKTSV4RRFFQ69G5FAZ", "name": "web"})
            )],
            &ServiceRemoveRowRequest {
                namespace_id: NamespaceRowId::try_new(CLUSTER).expect("namespace id"),
                name: "web".to_owned(),
                service_id: Some(ServiceRowId::try_new(ID_A).expect("id")),
            },
        ),
        Err(ServiceRemoveRowRefusal::StoredRowUnselectable {
            service_id: ServiceRowId::try_new(ID_A).expect("id"),
        })
    );

    let hostname = RouteHostname::try_new("web.example.com").expect("hostname");
    assert_eq!(
        select_route_removal(
            &cluster_id,
            vec![
                row(ID_B, named_document("route_bindings", hostname.as_str())),
                row(ID_A, named_document("route_bindings", hostname.as_str())),
            ],
            &RouteRemoveRequest {
                hostname: hostname.clone(),
                route_id: None,
            },
        ),
        Err(RouteRemoveRefusal::Ambiguous {
            hostname: hostname.clone(),
            route_ids: vec![
                RouteBindingRowId::try_new(ID_A).expect("id"),
                RouteBindingRowId::try_new(ID_B).expect("id"),
            ],
        })
    );
    assert_eq!(
        select_route_removal(
            &cluster_id,
            vec![row(
                ID_A,
                named_document("route_bindings", "other.example.com")
            )],
            &RouteRemoveRequest {
                hostname: hostname.clone(),
                route_id: Some(RouteBindingRowId::try_new(ID_A).expect("id")),
            },
        ),
        Err(RouteRemoveRefusal::NameMismatch {
            route_id: RouteBindingRowId::try_new(ID_A).expect("id"),
            requested: hostname,
            found: RouteHostname::try_new("other.example.com").expect("hostname"),
        })
    );
    assert!(matches!(
        select_route_removal(
            &cluster_id,
            vec![row(ID_A, Value::String("malformed".to_owned()))],
            &RouteRemoveRequest {
                hostname: RouteHostname::try_new("web.example.com").expect("hostname"),
                route_id: Some(RouteBindingRowId::try_new(ID_A).expect("id")),
            },
        ),
        Err(RouteRemoveRefusal::StoredRowUnselectable { route_id }) if route_id.as_str() == ID_A
    ));
}

#[test]
fn service_selection_uses_namespace_and_name_as_one_identity() {
    let cluster_id = ClusterId::try_new(CLUSTER).expect("cluster id");
    let request = ServiceRemoveRowRequest {
        namespace_id: NamespaceRowId::try_new(CLUSTER).expect("namespace id"),
        name: "web".to_owned(),
        service_id: None,
    };
    assert!(matches!(
        select_service_removal(
            &cluster_id,
            vec![
                row(ID_A, service_document(CLUSTER, "web")),
                row(ID_B, service_document(ID_C, "web")),
            ],
            &request,
        ),
        Ok(ServiceRemoveRowSelection::Delete { service_id, .. }) if service_id.as_str() == ID_A
    ));

    assert_eq!(
        select_service_removal(
            &cluster_id,
            vec![row(ID_B, service_document(ID_C, "web"))],
            &ServiceRemoveRowRequest {
                service_id: Some(ServiceRowId::try_new(ID_B).expect("service id")),
                ..request
            },
        ),
        Err(ServiceRemoveRowRefusal::IdentityMismatch {
            service_id: ServiceRowId::try_new(ID_B).expect("service id"),
            requested_namespace_id: NamespaceRowId::try_new(CLUSTER).expect("namespace id"),
            requested_name: "web".to_owned(),
            found_namespace_id: NamespaceRowId::try_new(ID_C).expect("namespace id"),
            found_name: "web".to_owned(),
        })
    );
}
