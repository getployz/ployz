use super::{
    HttpRouteTargetError, PingoraRouteRegistry, PingoraRouteSelectionError,
    route_target_from_authority,
};
use crate::roles::gateway::projection::{
    GatewayCertificateBundle, GatewayProjectedRoute, GatewayProjection, GatewayUpstream,
};
use pingora::protocols::l4::socket::SocketAddr as PingoraSocketAddr;
use ployz_core::certificate::{
    AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue, AcmeHttp01Challenge,
    ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow,
    CertificateChallengeApplicationStatus, CustomCertBundle, custom_bundle_digest,
};
use ployz_core::ids::RouteBindingId;
use ployz_core::ingress::{CertificateOwner, RouteBindingOrigin};
use ployz_core::operation::{RouteHostnameError, RouteTarget};
use ployz_test_support::ids::{cert_id, container_id, machine_id, route_hostname, route_port};
use rcgen::generate_simple_self_signed;
use std::collections::BTreeSet;
use std::net::SocketAddr;
use tokio::net::TcpListener;

#[test]
fn registry_serves_only_the_projected_http01_challenge() {
    let challenge = AcmeHttp01Challenge::try_new(
        ployz_test_support::ids::route_hostname("app.example.com"),
        AcmeChallengeToken::try_new("challenge-token").expect("valid token"),
        AcmeChallengeValue::try_new("challenge-token.account-thumbprint")
            .expect("valid key authorization"),
        AcmeChallengeTtlSeconds::try_new(900).expect("valid ttl"),
    )
    .expect("valid challenge");
    let registry = PingoraRouteRegistry::new();
    registry.apply_challenge(&challenge);

    assert_eq!(
        registry.http01_challenge("app.example.com", "challenge-token"),
        Some("challenge-token.account-thumbprint".to_owned())
    );
    assert_eq!(
        registry.http01_challenge("other.example.com", "challenge-token"),
        None
    );
    assert_eq!(
        registry.http01_challenge("app.example.com", "other-token"),
        None
    );
    assert_eq!(
        registry.challenge_application_status(&challenge),
        CertificateChallengeApplicationStatus::Applied
    );
    let stale = AcmeHttp01Challenge::try_new(
        route_hostname("app.example.com"),
        AcmeChallengeToken::try_new("stale-token").expect("token"),
        AcmeChallengeValue::try_new("stale-token.account-thumbprint").expect("value"),
        AcmeChallengeTtlSeconds::try_new(900).expect("ttl"),
    )
    .expect("challenge");
    assert_eq!(
        registry.challenge_application_status(&stale),
        CertificateChallengeApplicationStatus::NotApplied
    );
}

#[test]
fn registry_prepares_custom_tls_only_for_exact_projected_https_hostname() {
    let hostname = "app.example.com";
    let registry = PingoraRouteRegistry::new();
    registry
        .replace_projection(&GatewayProjection {
            certificate_bundles: vec![route_certificate_bundle(custom_bundle(hostname))],
            challenges: Vec::new(),
            routes: vec![projected_route_to_endpoint(
                hostname,
                443,
                "127.0.0.1",
                8080,
            )],
        })
        .expect("valid custom TLS projection");

    assert!(registry.is_https_hostname(hostname));
    assert!(!registry.is_https_hostname("other.example.com"));
}

#[test]
fn invalid_routed_custom_tls_retains_the_previous_snapshot() {
    let hostname = "app.example.com";
    let registry = PingoraRouteRegistry::new();
    registry
        .replace_projection(&GatewayProjection {
            certificate_bundles: vec![route_certificate_bundle(custom_bundle(hostname))],
            challenges: Vec::new(),
            routes: vec![projected_route_to_endpoint(
                hostname,
                443,
                "127.0.0.1",
                8080,
            )],
        })
        .expect("valid custom TLS projection");
    let (_, valid_private_key) = certificate_material(hostname);
    let invalid =
        custom_bundle_with_material(hostname, "not a certificate".to_owned(), valid_private_key);

    assert!(
        registry
            .replace_projection(&GatewayProjection {
                certificate_bundles: vec![route_certificate_bundle(invalid)],
                challenges: Vec::new(),
                routes: vec![projected_route_to_endpoint(
                    hostname,
                    443,
                    "127.0.0.1",
                    9090,
                )],
            })
            .is_err()
    );
    assert_eq!(
        (
            registry.is_https_hostname(hostname),
            selected_addr(&registry, &route_target(hostname, 443)),
        ),
        (true, socket_addr("127.0.0.1:8080"))
    );
}

#[test]
fn invalid_unrouted_custom_tls_rejects_replacement_and_retains_last_good() {
    let hostname = "app.example.com";
    let target = route_target(hostname, 443);
    let registry = PingoraRouteRegistry::new();
    registry
        .replace_projection(&GatewayProjection {
            certificate_bundles: vec![route_certificate_bundle(custom_bundle(hostname))],
            challenges: Vec::new(),
            routes: vec![projected_route_to_endpoint(
                hostname,
                443,
                "127.0.0.1",
                8080,
            )],
        })
        .expect("valid custom TLS projection");
    let (_, valid_private_key) = certificate_material("unrouted.example.com");
    let invalid = custom_bundle_with_material(
        "unrouted.example.com",
        "not a certificate".to_owned(),
        valid_private_key,
    );

    assert!(
        registry
            .replace_projection(&GatewayProjection {
                certificate_bundles: vec![route_certificate_bundle(invalid)],
                challenges: Vec::new(),
                routes: Vec::new(),
            })
            .is_err()
    );

    assert_eq!(
        (
            registry.is_https_hostname(hostname),
            registry.backend_count(&target)
        ),
        (true, 1)
    );
}

fn custom_bundle(hostname: &str) -> CustomCertBundle {
    let (certificate_chain_pem, private_key_pem) = certificate_material(hostname);
    custom_bundle_with_material(hostname, certificate_chain_pem, private_key_pem)
}

fn certificate_material(hostname: &str) -> (String, String) {
    let rcgen::CertifiedKey { cert, signing_key } =
        generate_simple_self_signed([hostname.to_owned()]).expect("fixture certificate");
    (cert.pem(), signing_key.serialize_pem())
}

fn custom_bundle_with_material(
    hostname: &str,
    certificate_chain_pem: String,
    private_key_pem: String,
) -> CustomCertBundle {
    let digest = custom_bundle_digest(&certificate_chain_pem, &private_key_pem).expect("digest");
    CustomCertBundle::try_new(
        ActiveCertState {
            cert_id: cert_id("cert_custom"),
            hostname: route_hostname(hostname),
            bundle_ref: CertBundleRef::try_new(format!(
                "sha256:{}:/var/lib/ployz/certs/cert_custom.pem",
                digest.as_str()
            ))
            .expect("valid bundle ref"),
            validity: CertValidityWindow::try_new(
                CertValidAt::try_new(1).expect("valid not-before"),
                CertValidAt::try_new(2).expect("valid not-after"),
            )
            .expect("valid certificate window"),
        },
        certificate_chain_pem,
        private_key_pem,
    )
    .expect("bundle digest matches material")
}

#[test]
fn pingora_registry_replaces_routes_from_projection() {
    let registry = PingoraRouteRegistry::new();

    registry
        .replace_projection(&projection([
            projected_route_to_endpoint("api.example.com", 80, "127.0.0.1", 8080),
            projected_route_to_endpoint("admin.example.com", 80, "127.0.0.1", 8081),
        ]))
        .expect("registry accepts projection");
    selected_addr(&registry, &route_target("api.example.com", 80));
    selected_addr(&registry, &route_target("admin.example.com", 80));

    registry
        .replace_projection(&projection([projected_route_to_endpoint(
            "api.example.com",
            80,
            "127.0.0.1",
            9090,
        )]))
        .expect("registry accepts replacement projection");

    selected_addr(&registry, &route_target("api.example.com", 80));
    assert_eq!(
        registry
            .select_backend(&route_target("admin.example.com", 80), &BTreeSet::new())
            .expect_err("removed route is not selectable"),
        PingoraRouteSelectionError::NoRoute {
            target: route_target("admin.example.com", 80)
        }
    );
}

#[test]
fn pingora_registry_load_balances_across_route_upstreams() {
    let registry = PingoraRouteRegistry::new();
    let target = route_target("api.example.com", 80);
    registry
        .replace_projection(&projection([GatewayProjectedRoute {
            id: route_binding_id(),
            origin: RouteBindingOrigin::Declared,
            target: target.clone(),
            upstreams: vec![
                upstream_to_endpoint("127.0.0.1", 8080),
                upstream_to_endpoint("127.0.0.1", 8081),
            ],
            unroutable_containers: vec![],
        }]))
        .expect("registry accepts projection");

    let first = selected_addr(&registry, &target);
    let second = selected_addr(&registry, &target);

    assert_ne!(first, second);
    assert_eq!(
        BTreeSet::from([first, second]),
        BTreeSet::from([socket_addr("127.0.0.1:8080"), socket_addr("127.0.0.1:8081")])
    );
}

#[tokio::test]
async fn pingora_registry_health_checks_hide_refused_upstreams() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind temporary port");
    let refused_addr = listener.local_addr().expect("temporary listener addr");
    drop(listener);

    let registry = PingoraRouteRegistry::new();
    let target = route_target("api.example.com", 80);
    registry
        .replace_projection(&projection([projected_route_to_endpoint(
            "api.example.com",
            80,
            "127.0.0.1",
            refused_addr.port(),
        )]))
        .expect("registry accepts projection");

    registry.run_health_checks().await;

    assert_eq!(
        registry
            .select_backend(&target, &BTreeSet::new())
            .expect_err("refused upstream becomes unavailable"),
        PingoraRouteSelectionError::NoHealthyUpstream { target }
    );
}

#[test]
fn http_authority_defaults_to_listener_port() {
    assert_eq!(
        route_target_from_authority("API.example.com", route_port(80)).expect("authority is valid"),
        route_target("api.example.com", 80)
    );
}

#[test]
fn http_authority_accepts_explicit_port() {
    assert_eq!(
        route_target_from_authority("api.example.com:8443", route_port(80))
            .expect("authority is valid"),
        route_target("api.example.com", 8443)
    );
}

#[test]
fn http_authority_rejects_empty_host() {
    assert_eq!(
        route_target_from_authority("  ", route_port(80)).expect_err("empty host is rejected"),
        HttpRouteTargetError::EmptyHost
    );
}

#[test]
fn http_authority_rejects_user_info() {
    assert_eq!(
        route_target_from_authority("user@api.example.com", route_port(80))
            .expect_err("user info is rejected"),
        HttpRouteTargetError::UserInfo
    );
}

#[test]
fn http_authority_rejects_missing_host_before_port() {
    assert_eq!(
        route_target_from_authority(":443", route_port(80)).expect_err("missing host is rejected"),
        HttpRouteTargetError::MissingHost
    );
}

#[test]
fn http_authority_rejects_missing_port_after_colon() {
    assert_eq!(
        route_target_from_authority("api.example.com:", route_port(80))
            .expect_err("missing port is rejected"),
        HttpRouteTargetError::MissingPort
    );
}

#[test]
fn http_authority_rejects_invalid_port() {
    assert_eq!(
        route_target_from_authority("api.example.com:not-a-port", route_port(80))
            .expect_err("invalid port is rejected"),
        HttpRouteTargetError::InvalidPort
    );
    assert_eq!(
        route_target_from_authority("api.example.com:0", route_port(80))
            .expect_err("zero port is rejected"),
        HttpRouteTargetError::InvalidPort
    );
}

#[test]
fn http_authority_rejects_ipv6_for_now() {
    assert_eq!(
        route_target_from_authority("[::1]:443", route_port(80))
            .expect_err("ipv6 is not supported yet"),
        HttpRouteTargetError::UnsupportedIpv6
    );
}

#[test]
fn http_authority_rejects_invalid_hostname() {
    assert_eq!(
        route_target_from_authority("-api.example.com", route_port(80))
            .expect_err("invalid hostname is rejected"),
        HttpRouteTargetError::InvalidHostname {
            source: RouteHostnameError::Invalid {
                value: "-api.example.com".to_owned(),
            },
        }
    );
}

fn selected_addr(registry: &PingoraRouteRegistry, target: &RouteTarget) -> SocketAddr {
    let backend = registry
        .select_backend(target, &BTreeSet::new())
        .expect("backend is selected");
    match backend.addr {
        PingoraSocketAddr::Inet(addr) => addr,
        PingoraSocketAddr::Unix(_) => panic!("gateway tests only use TCP backends"),
    }
}

fn projection(routes: impl IntoIterator<Item = GatewayProjectedRoute>) -> GatewayProjection {
    GatewayProjection {
        certificate_bundles: Vec::new(),
        challenges: Vec::new(),
        routes: routes.into_iter().collect(),
    }
}

fn projected_route_to_endpoint(
    hostname: &str,
    port: u16,
    endpoint_ip: &str,
    endpoint_port: u16,
) -> GatewayProjectedRoute {
    GatewayProjectedRoute {
        id: route_binding_id(),
        origin: RouteBindingOrigin::Declared,
        target: route_target(hostname, port),
        upstreams: vec![upstream_to_endpoint(endpoint_ip, endpoint_port)],
        unroutable_containers: vec![],
    }
}

fn upstream_to_endpoint(ip: &str, port: u16) -> GatewayUpstream {
    GatewayUpstream {
        machine_id: machine_id("machine_1"),
        container_id: container_id("ctr_1"),
        address: SocketAddr::new(ip.parse().expect("valid endpoint ip"), port),
    }
}

fn route_target(hostname: &str, _port: u16) -> RouteTarget {
    RouteTarget::new(route_hostname(hostname))
}

fn route_binding_id() -> RouteBindingId {
    RouteBindingId::try_new("route_app").expect("valid route binding id")
}

fn route_certificate_bundle(bundle: CustomCertBundle) -> GatewayCertificateBundle {
    GatewayCertificateBundle {
        owner: CertificateOwner::RouteBinding {
            route_binding_id: route_binding_id(),
        },
        bundle,
    }
}

fn socket_addr(value: &str) -> SocketAddr {
    value.parse().expect("valid socket address")
}
