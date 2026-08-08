use ployz_core::corrosion::{
    CorrosionNamespaceName, CorrosionServiceName, IngressMode, Principal, RouteBindingDocument,
};
use ployz_core::ids::{NamespaceRowId, PeerId, RouteBindingRowId, ServiceRowId};
use ployz_core::operation::{RouteHostname, RoutePort};
use ployz_core::{
    KnownApiFeature, ROUTE_ATTACH_ROUTE, RouteAttachIntent, RouteAttachOutcome, RouteAttachRefusal,
    RouteAttachReply, RouteAttachRequest, RouteRemoveRequest, V2Method, V2Route,
};

const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

#[test]
fn route_attach_has_one_post_peer_only_advertised_surface() {
    let route = V2Route::parse(ROUTE_ATTACH_ROUTE).expect("route attach route");

    assert_eq!(route, V2Route::RouteAttach);
    assert_eq!(route.path(), ROUTE_ATTACH_ROUTE);
    assert_eq!(route.method(), V2Method::Post);
    assert_eq!(route.feature(), KnownApiFeature::RouteAttach);
    assert!(route.accepts_principal(&Principal::Peer {
        peer_id: PeerId::try_new(ID).expect("peer id"),
    }));
    assert!(!route.accepts_principal(&Principal::Machine {
        machine_id: ployz_core::ids::MachineRowId::try_new(ID).expect("machine id"),
    }));
    assert!(!route.accepts_principal(&Principal::ApiToken {
        token_id: ployz_core::ids::TokenId::try_new(ID).expect("token id"),
    }));
}

#[test]
fn route_attach_request_carries_named_and_optional_exact_id_selectors() {
    let value = serde_json::json!({
        "hostname": "WEB.EXAMPLE.COM",
        "namespace_name": "production",
        "namespace_id": ID,
        "service_name": "web",
        "service_id": ID,
        "endpoint_port": 8080,
        "ingress_mode": "direct"
    });

    let request: RouteAttachRequest = serde_json::from_value(value).expect("request");

    assert_eq!(request.hostname.as_str(), "web.example.com");
    assert_eq!(request.namespace_name.as_str(), "production");
    assert_eq!(request.namespace_id.expect("namespace id").as_str(), ID);
    assert_eq!(request.service_name.as_str(), "web");
    assert_eq!(request.service_id.expect("service id").as_str(), ID);
    assert_eq!(request.endpoint_port.get(), 8080);
    assert_eq!(request.ingress_mode, IngressMode::Direct);
}

#[test]
fn non_direct_ingress_has_a_typed_attach_refusal() {
    assert_eq!(
        serde_json::to_value(RouteAttachRefusal::UnsupportedIngressMode {
            requested: IngressMode::CloudflareTunnel,
        })
        .expect("refusal"),
        serde_json::json!({
            "kind": "unsupported_ingress_mode",
            "requested": "cloudflare_tunnel"
        })
    );
}

#[test]
fn route_attach_reply_distinguishes_new_and_identical_existing_bindings() {
    let route_id = RouteBindingRowId::try_new(ID).expect("route id");

    for (outcome, wire_kind) in [
        (RouteAttachOutcome::Attached, "attached"),
        (RouteAttachOutcome::AlreadyAttached, "already_attached"),
    ] {
        let reply = RouteAttachReply {
            route_id: route_id.clone(),
            outcome,
        };
        assert_eq!(
            serde_json::to_value(reply)
                .expect("reply")
                .get("outcome")
                .and_then(serde_json::Value::as_str),
            Some(wire_kind)
        );
    }
}

#[test]
fn hostname_collision_carries_the_exact_route_removal_handoff() {
    let hostname = RouteHostname::try_new("web.example.com").expect("hostname");
    let route_id = RouteBindingRowId::try_new(ID).expect("route id");
    let refusal = RouteAttachRefusal::HostnameAlreadyAttached {
        hostname: hostname.clone(),
        route_id: route_id.clone(),
        remove: RouteRemoveRequest {
            hostname,
            route_id: Some(route_id),
        },
    };

    assert_eq!(
        serde_json::to_value(refusal).expect("refusal"),
        serde_json::json!({
            "kind": "hostname_already_attached",
            "hostname": "web.example.com",
            "route_id": ID,
            "remove": {"hostname": "web.example.com", "route_id": ID}
        })
    );
}

#[test]
fn route_attach_selection_refusals_keep_names_and_exact_identity_evidence() {
    let namespace_name = CorrosionNamespaceName::try_new("production").expect("namespace");
    let service_name = CorrosionServiceName::try_new("web").expect("service");
    let namespace_id = NamespaceRowId::try_new(ID).expect("namespace id");
    let service_id = ServiceRowId::try_new(ID).expect("service id");
    let port = RoutePort::try_new(80).expect("port");

    let refusals = [
        RouteAttachRefusal::NamespaceNotFound {
            namespace_name: namespace_name.clone(),
        },
        RouteAttachRefusal::NamespaceIdMismatch {
            namespace_name,
            requested: namespace_id.clone(),
            found: namespace_id.clone(),
        },
        RouteAttachRefusal::ServiceNotFound {
            namespace_id: namespace_id.clone(),
            service_name: service_name.clone(),
        },
        RouteAttachRefusal::ServiceIdMismatch {
            namespace_id,
            service_name,
            requested: service_id.clone(),
            found: service_id,
        },
    ];

    for refusal in refusals {
        serde_json::to_value(refusal).expect("typed refusal serializes");
    }
    assert_eq!(port.get(), 80);
}

#[test]
fn idempotence_requires_the_complete_declared_route_intent() {
    let document = serde_json::from_value::<RouteBindingDocument>(serde_json::json!({
        "v": 1,
        "cluster_id": ID,
        "written_by": {"kind": "peer", "peer_id": ID},
        "written_at": "2026-08-08T10:00:00Z",
        "hostname": "web.example.com",
        "namespace_id": ID,
        "service_id": ID,
        "endpoint_port": 8080,
        "origin": "declared",
        "ingress_mode": "direct"
    }))
    .expect("route document");
    let intent = RouteAttachIntent {
        hostname: RouteHostname::try_new("web.example.com").expect("hostname"),
        namespace_id: NamespaceRowId::try_new(ID).expect("namespace id"),
        service_id: ServiceRowId::try_new(ID).expect("service id"),
        endpoint_port: RoutePort::try_new(8080).expect("port"),
        ingress_mode: IngressMode::Direct,
    };

    assert!(intent.matches(&document));

    let mut different_port = document.clone();
    different_port.endpoint_port = RoutePort::try_new(8081).expect("port");
    assert!(!intent.matches(&different_port));

    let mut automatic = document;
    automatic.origin = ployz_core::ingress::RouteBindingOrigin::Automatic;
    assert!(!intent.matches(&automatic));
}
