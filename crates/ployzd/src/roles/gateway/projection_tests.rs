use super::{
    GatewayCertificateBundle, GatewayCertificateFailureAvailability,
    GatewayCertificateMaterialFailure, GatewayCertificateMaterialFailureKind,
    GatewayProjectedRoute, GatewayProjection, GatewayProjectionError, GatewayProjectionInput,
    GatewayProjectionState, GatewayProjectionUpdate, GatewayRoute, GatewayServingEntry,
    GatewayUnroutableContainer, GatewayUpstream, apply_gateway_update, project_gateway,
};
use ployz_core::certificateificate::{
    AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue, AcmeHttp01Challenge,
    ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow, CustomCertBundle,
    custom_bundle_digest,
};
use ployz_core::ids::{CertId, RouteBindingId};
use ployz_core::ingress::{CertificateOwner, RouteBindingOrigin};
use ployz_core::machine::runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, ManagedContainerKind,
    ManagedContainerObservation,
};
use ployz_core::operation::RouteTarget;
use ployz_test_support::containers;
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, namespace_revision_entry_id, route_hostname,
    route_port, service_id,
};
use std::net::SocketAddr;

#[test]
fn gateway_projection_carries_http01_challenges_without_routes() {
    let challenge = AcmeHttp01Challenge::try_new(
        route_hostname("app.example.com"),
        AcmeChallengeToken::try_new("challenge-token").expect("valid token"),
        AcmeChallengeValue::try_new("challenge-token.account-thumbprint")
            .expect("valid key authorization"),
        AcmeChallengeTtlSeconds::try_new(900).expect("valid ttl"),
    )
    .expect("valid challenge");

    let projection = project_gateway(GatewayProjectionInput {
        certificate_bundles: Vec::new(),
        certificate_failures: Vec::new(),
        challenges: vec![challenge.clone()],
        routes: Vec::new(),
        serving: Vec::new(),
        observed_machines: Vec::new(),
    })
    .expect("valid gateway projection");

    assert_eq!(projection.challenges, vec![challenge]);
}

#[test]
fn failed_custom_material_retains_prior_tls_and_applies_unrelated_updates() {
    let hostname = route_hostname("broken.example.com");
    let prior_bundle = custom_bundle(hostname.as_str());
    let challenge = http01_challenge(hostname.as_str());
    let failure = GatewayCertificateMaterialFailure {
        owner: route_owner(hostname.as_str()),
        kind: GatewayCertificateMaterialFailureKind::MissingOrInvalid,
        message: "local bundle is corrupt".to_owned(),
    };
    let previous = GatewayProjectionState {
        last_good: Some(GatewayProjection {
            certificate_bundles: vec![route_certificate_bundle(
                hostname.as_str(),
                prior_bundle.clone(),
            )],
            challenges: Vec::new(),
            routes: Vec::new(),
        }),
        last_error: None,
    };

    let state = apply_gateway_update(
        previous,
        GatewayProjectionUpdate::SourceAvailable(Box::new(GatewayProjectionInput {
            certificate_bundles: Vec::new(),
            certificate_failures: vec![failure.clone()],
            challenges: vec![challenge.clone()],
            routes: vec![
                gateway_route("broken.example.com", "svc_broken"),
                gateway_route("healthy.example.com", "svc_healthy"),
            ],
            serving: Vec::new(),
            observed_machines: Vec::new(),
        })),
    );

    let projection = state.last_good.expect("degraded source still applies");
    assert_eq!(
        (
            projection.certificate_bundles,
            projection.challenges,
            projection
                .routes
                .into_iter()
                .map(|route| route.target)
                .collect::<Vec<_>>(),
            state.last_error,
        ),
        (
            vec![route_certificate_bundle(hostname.as_str(), prior_bundle)],
            vec![challenge],
            vec![
                route_target("broken.example.com", 443),
                route_target("healthy.example.com", 443),
            ],
            Some(GatewayProjectionError::CertificateMaterial {
                failures: vec![failure],
                availability: GatewayCertificateFailureAvailability::ServingLastKnownGood,
            }),
        )
    );
}

#[test]
fn failed_custom_material_on_cold_start_suppresses_only_affected_https_route() {
    let projection = project_gateway(GatewayProjectionInput {
        certificate_bundles: Vec::new(),
        certificate_failures: vec![GatewayCertificateMaterialFailure {
            owner: route_owner("broken.example.com"),
            kind: GatewayCertificateMaterialFailureKind::MissingOrInvalid,
            message: "local bundle is missing".to_owned(),
        }],
        challenges: vec![http01_challenge("broken.example.com")],
        routes: vec![
            gateway_route("broken.example.com", "svc_broken"),
            gateway_route("healthy.example.com", "svc_healthy"),
        ],
        serving: Vec::new(),
        observed_machines: Vec::new(),
    })
    .expect("one certificate failure does not invalidate the source");

    assert_eq!(
        projection
            .routes
            .into_iter()
            .map(|route| route.target)
            .collect::<Vec<_>>(),
        vec![route_target("healthy.example.com", 443)]
    );
}

#[test]
fn gateway_serves_every_observed_machine_and_filters_non_running_upstreams() {
    let projection = project_gateway(GatewayProjectionInput {
        certificate_bundles: Vec::new(),
        certificate_failures: Vec::new(),
        challenges: Vec::new(),
        routes: vec![
            gateway_route("WWW.example.com", "svc_web"),
            gateway_route("api.example.com", "svc_api"),
        ],
        serving: vec![
            serving_entry("svc_web", "entry_1"),
            serving_entry("svc_api", "entry_2"),
        ],
        observed_machines: vec![
            observed_machine(
                "machine_1",
                vec![
                    service_container("machine_1", "api_good", "svc_api", "entry_2", running()),
                    service_container("machine_1", "web_good", "svc_web", "entry_1", running()),
                    service_container("machine_1", "api_exited", "svc_api", "entry_2", exited()),
                    service_container("machine_1", "api_old", "svc_api", "entry_1", running()),
                    managed_container(
                        "machine_1",
                        "api_job",
                        "svc_api",
                        "entry_2",
                        ManagedContainerKind::Job,
                        running(),
                    ),
                ],
            ),
            observed_machine(
                "machine_2",
                vec![service_container(
                    "machine_2",
                    "api_offline_later",
                    "svc_api",
                    "entry_2",
                    running(),
                )],
            ),
        ],
    })
    .expect("valid gateway projection");

    assert_eq!(
        projection,
        GatewayProjection {
            certificate_bundles: Vec::new(),
            challenges: Vec::new(),
            routes: vec![
                GatewayProjectedRoute {
                    id: route_binding_id("api.example.com"),
                    origin: RouteBindingOrigin::Declared,
                    target: route_target("api.example.com", 443),
                    upstreams: vec![
                        GatewayUpstream {
                            machine_id: machine_id("machine_1"),
                            container_id: container_id("api_good"),
                            address: socket_addr("10.0.0.1", 8080),
                        },
                        GatewayUpstream {
                            machine_id: machine_id("machine_2"),
                            container_id: container_id("api_offline_later"),
                            address: socket_addr("10.0.0.1", 8080),
                        },
                    ],
                    unroutable_containers: vec![],
                },
                GatewayProjectedRoute {
                    id: route_binding_id("www.example.com"),
                    origin: RouteBindingOrigin::Declared,
                    target: route_target("www.example.com", 443),
                    upstreams: vec![GatewayUpstream {
                        machine_id: machine_id("machine_1"),
                        container_id: container_id("web_good"),
                        address: socket_addr("10.0.0.1", 8080),
                    }],
                    unroutable_containers: vec![],
                },
            ],
        }
    );
}

#[test]
fn gateway_filters_running_containers_without_endpoint_evidence() {
    let projection = project_gateway(GatewayProjectionInput {
        certificate_bundles: Vec::new(),
        certificate_failures: Vec::new(),
        challenges: Vec::new(),
        routes: vec![gateway_route("api.example.com", "svc_api")],
        serving: vec![serving_entry("svc_api", "entry_2")],
        observed_machines: vec![observed_machine(
            "machine_1",
            vec![service_container(
                "machine_1",
                "api_unroutable",
                "svc_api",
                "entry_2",
                ContainerRuntimeState::running_unroutable(),
            )],
        )],
    })
    .expect("valid gateway projection");

    assert_eq!(
        projection,
        GatewayProjection {
            certificate_bundles: Vec::new(),
            challenges: Vec::new(),
            routes: vec![GatewayProjectedRoute {
                id: route_binding_id("api.example.com"),
                origin: RouteBindingOrigin::Declared,
                target: route_target("api.example.com", 443),
                upstreams: vec![],
                unroutable_containers: vec![GatewayUnroutableContainer {
                    machine_id: machine_id("machine_1"),
                    container_id: container_id("api_unroutable"),
                }],
            }],
        }
    );
}

#[test]
fn gateway_dials_matching_containers_on_the_route_endpoint_port() {
    // The container's own created port never participates in matching:
    // every serving-entry container is dialed on the route's endpoint port
    // (ADR 0023), so an endpoint reroute needs no container replacement.
    let projection = project_gateway(GatewayProjectionInput {
        certificate_bundles: Vec::new(),
        certificate_failures: Vec::new(),
        challenges: Vec::new(),
        routes: vec![gateway_route("api.example.com", "svc_api")],
        serving: vec![serving_entry("svc_api", "entry_2")],
        observed_machines: vec![observed_machine(
            "machine_1",
            vec![
                service_container(
                    "machine_1",
                    "api_one",
                    "svc_api",
                    "entry_2",
                    ContainerRuntimeState::running_at(endpoint_ip("10.0.0.1")),
                ),
                service_container(
                    "machine_1",
                    "api_two",
                    "svc_api",
                    "entry_2",
                    ContainerRuntimeState::running_at(endpoint_ip("10.0.0.2")),
                ),
            ],
        )],
    })
    .expect("valid gateway projection");

    assert_eq!(
        projection,
        GatewayProjection {
            certificate_bundles: Vec::new(),
            challenges: Vec::new(),
            routes: vec![GatewayProjectedRoute {
                id: route_binding_id("api.example.com"),
                origin: RouteBindingOrigin::Declared,
                target: route_target("api.example.com", 443),
                upstreams: vec![
                    GatewayUpstream {
                        machine_id: machine_id("machine_1"),
                        container_id: container_id("api_one"),
                        address: socket_addr("10.0.0.1", 8080),
                    },
                    GatewayUpstream {
                        machine_id: machine_id("machine_1"),
                        container_id: container_id("api_two"),
                        address: socket_addr("10.0.0.2", 8080),
                    },
                ],
                unroutable_containers: vec![],
            }],
        }
    );
}

#[test]
fn gateway_keeps_route_with_no_upstreams_when_service_is_not_serving() {
    // A binding whose service is absent from the serving target stays
    // attached and unavailable instead of becoming invalid state (ADR 0024).
    let projection = project_gateway(GatewayProjectionInput {
        certificate_bundles: Vec::new(),
        certificate_failures: Vec::new(),
        challenges: Vec::new(),
        routes: vec![gateway_route("api.example.com", "svc_api")],
        serving: vec![],
        observed_machines: vec![observed_machine(
            "machine_1",
            vec![service_container(
                "machine_1",
                "api_orphan",
                "svc_api",
                "entry_2",
                running(),
            )],
        )],
    })
    .expect("valid gateway projection");

    assert_eq!(
        projection,
        GatewayProjection {
            certificate_bundles: Vec::new(),
            challenges: Vec::new(),
            routes: vec![GatewayProjectedRoute {
                id: route_binding_id("api.example.com"),
                origin: RouteBindingOrigin::Declared,
                target: route_target("api.example.com", 443),
                upstreams: vec![],
                unroutable_containers: vec![],
            }],
        }
    );
}

#[test]
fn gateway_ignores_containers_with_a_different_entry_identity() {
    let projection = project_gateway(GatewayProjectionInput {
        certificate_bundles: Vec::new(),
        certificate_failures: Vec::new(),
        challenges: Vec::new(),
        routes: vec![gateway_route("api.example.com", "svc_api")],
        serving: vec![serving_entry("svc_api", "entry_2")],
        observed_machines: vec![observed_machine(
            "machine_1",
            vec![service_container(
                "machine_1",
                "api_old",
                "svc_api",
                "entry_old",
                running(),
            )],
        )],
    })
    .expect("valid gateway projection");

    assert_eq!(
        projection,
        GatewayProjection {
            certificate_bundles: Vec::new(),
            challenges: Vec::new(),
            routes: vec![GatewayProjectedRoute {
                id: route_binding_id("api.example.com"),
                origin: RouteBindingOrigin::Declared,
                target: route_target("api.example.com", 443),
                upstreams: vec![],
                unroutable_containers: vec![],
            }],
        }
    );
}

#[test]
fn gateway_keeps_last_good_projection_when_source_is_unavailable() {
    let error = source_unavailable();
    let last_good = single_route_projection();

    assert_eq!(
        apply_gateway_update(
            current_state(last_good),
            GatewayProjectionUpdate::SourceUnavailable(error.clone()),
        ),
        failed_state(single_route_projection(), error)
    );
    assert_eq!(
        apply_gateway_update(
            GatewayProjectionState::unavailable(),
            GatewayProjectionUpdate::SourceUnavailable(source_unavailable()),
        ),
        GatewayProjectionState {
            last_good: None,
            last_error: Some(source_unavailable()),
        }
    );
}

#[test]
fn gateway_rejects_duplicate_route_targets() {
    let target = route_target("api.example.com", 443);
    assert_eq!(
        project_gateway(GatewayProjectionInput {
            certificate_bundles: Vec::new(),
            certificate_failures: Vec::new(),
            challenges: Vec::new(),
            routes: vec![
                GatewayRoute {
                    id: route_binding_id("API.example.com"),
                    origin: RouteBindingOrigin::Declared,
                    certificate_owner: route_owner("API.example.com"),
                    namespace_id: namespace_id("default"),
                    target: route_target("API.example.com", 443),
                    endpoint_port: route_port(8080),
                    service_id: service_id("svc_api"),
                },
                GatewayRoute {
                    id: route_binding_id("api.example.com"),
                    origin: RouteBindingOrigin::Declared,
                    certificate_owner: route_owner("api.example.com"),
                    namespace_id: namespace_id("default"),
                    target: target.clone(),
                    endpoint_port: route_port(8080),
                    service_id: service_id("svc_api"),
                },
            ],
            serving: vec![],
            observed_machines: vec![],
        }),
        Err(GatewayProjectionError::DuplicateRouteTarget { target })
    );
}

#[test]
fn gateway_retains_last_good_projection_when_source_is_invalid() {
    let target = route_target("api.example.com", 443);
    let last_good = single_route_projection();
    let update = GatewayProjectionUpdate::SourceAvailable(Box::new(GatewayProjectionInput {
        certificate_bundles: Vec::new(),
        certificate_failures: Vec::new(),
        challenges: Vec::new(),
        routes: vec![
            GatewayRoute {
                id: route_binding_id("api.example.com"),
                origin: RouteBindingOrigin::Declared,
                certificate_owner: route_owner("api.example.com"),
                namespace_id: namespace_id("default"),
                target: target.clone(),
                endpoint_port: route_port(8080),
                service_id: service_id("svc_api"),
            },
            GatewayRoute {
                id: route_binding_id("api.example.com"),
                origin: RouteBindingOrigin::Declared,
                certificate_owner: route_owner("api.example.com"),
                namespace_id: namespace_id("default"),
                target: target.clone(),
                endpoint_port: route_port(8080),
                service_id: service_id("svc_api"),
            },
        ],
        serving: vec![],
        observed_machines: vec![],
    }));

    assert_eq!(
        apply_gateway_update(current_state(last_good), update),
        failed_state(
            single_route_projection(),
            GatewayProjectionError::DuplicateRouteTarget { target },
        )
    );
}

#[test]
fn gateway_retains_last_good_projection_when_source_decode_fails() {
    let last_good = single_route_projection();
    let error = GatewayProjectionError::InvalidSource {
        message: "route decode failed".to_owned(),
    };

    assert_eq!(
        apply_gateway_update(
            current_state(last_good),
            GatewayProjectionUpdate::SourceInvalid(error.clone()),
        ),
        failed_state(single_route_projection(), error)
    );
}

#[test]
fn gateway_keeps_failure_evidence_when_invalid_source_then_disappears() {
    let last_good = single_route_projection();
    let error = GatewayProjectionError::InvalidSource {
        message: "route decode failed".to_owned(),
    };

    let failed = apply_gateway_update(
        current_state(last_good),
        GatewayProjectionUpdate::SourceInvalid(error.clone()),
    );

    assert_eq!(
        apply_gateway_update(
            failed,
            GatewayProjectionUpdate::SourceUnavailable(source_unavailable()),
        ),
        failed_state(single_route_projection(), error)
    );
}

fn single_route_projection() -> GatewayProjection {
    GatewayProjection {
        certificate_bundles: Vec::new(),
        challenges: Vec::new(),
        routes: vec![GatewayProjectedRoute {
            id: route_binding_id("api.example.com"),
            origin: RouteBindingOrigin::Declared,
            target: route_target("api.example.com", 443),
            upstreams: vec![GatewayUpstream {
                machine_id: machine_id("machine_1"),
                container_id: container_id("api_good"),
                address: socket_addr("10.0.0.1", 8080),
            }],
            unroutable_containers: vec![],
        }],
    }
}

fn observed_machine(
    machine_id_value: &str,
    containers: Vec<ManagedContainerObservation>,
) -> MachineContainerObservationSnapshot {
    MachineContainerObservationSnapshot::try_new(machine_id(machine_id_value), containers)
        .expect("valid machine snapshot")
}

fn gateway_route(hostname: &str, service_id_value: &str) -> GatewayRoute {
    GatewayRoute {
        id: route_binding_id(hostname),
        origin: RouteBindingOrigin::Declared,
        certificate_owner: route_owner(hostname),
        namespace_id: namespace_id("default"),
        target: route_target(hostname, 443),
        endpoint_port: route_port(8080),
        service_id: service_id(service_id_value),
    }
}

fn serving_entry(
    service_id_value: &str,
    namespace_revision_entry_id_value: &str,
) -> GatewayServingEntry {
    GatewayServingEntry {
        namespace_id: namespace_id("default"),
        service_id: service_id(service_id_value),
        namespace_revision_entry_id: namespace_revision_entry_id(namespace_revision_entry_id_value),
    }
}

fn service_container(
    machine_id_value: &str,
    container_id_value: &str,
    service_id_value: &str,
    namespace_revision_entry_id_value: &str,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    managed_container(
        machine_id_value,
        container_id_value,
        service_id_value,
        namespace_revision_entry_id_value,
        ManagedContainerKind::Service,
        state,
    )
}

fn managed_container(
    machine_id_value: &str,
    container_id_value: &str,
    service_id_value: &str,
    namespace_revision_entry_id_value: &str,
    kind: ManagedContainerKind,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    containers::observation(machine_id_value, container_id_value)
        .with(
            containers::identity(service_id_value)
                .entry(namespace_revision_entry_id_value)
                .operation("op_1")
                .step("step_1")
                .kind(kind),
        )
        .state(state)
        .build()
}

fn route_target(hostname: &str, _port: u16) -> RouteTarget {
    RouteTarget::new(route_hostname(hostname))
}

fn route_binding_id(hostname: &str) -> RouteBindingId {
    RouteBindingId::try_new(format!(
        "route_{}",
        hostname.to_ascii_lowercase().replace('.', "_")
    ))
    .expect("valid route binding id")
}

fn route_owner(hostname: &str) -> CertificateOwner {
    CertificateOwner::RouteBinding {
        route_binding_id: route_binding_id(hostname),
    }
}

fn route_certificate_bundle(hostname: &str, bundle: CustomCertBundle) -> GatewayCertificateBundle {
    GatewayCertificateBundle {
        owner: route_owner(hostname),
        bundle,
    }
}

fn custom_bundle(hostname: &str) -> CustomCertBundle {
    let certificate = format!("certificate for {hostname}");
    let private_key = format!("private key for {hostname}");
    let digest = custom_bundle_digest(&certificate, &private_key).expect("bundle digest");
    CustomCertBundle::try_new(
        ActiveCertState {
            cert_id: CertId::try_new(format!("cert_{}", hostname.replace('.', "_")))
                .expect("cert id"),
            hostname: route_hostname(hostname),
            bundle_ref: CertBundleRef::try_new(format!(
                "sha256:{}:/var/lib/ployz/certificates/bundle.pem",
                digest.as_str()
            ))
            .expect("bundle reference"),
            validity: CertValidityWindow::try_new(
                CertValidAt::try_new(1).expect("not before"),
                CertValidAt::try_new(2).expect("not after"),
            )
            .expect("validity"),
        },
        certificate,
        private_key,
    )
    .expect("custom bundle")
}

fn http01_challenge(hostname: &str) -> AcmeHttp01Challenge {
    AcmeHttp01Challenge::try_new(
        route_hostname(hostname),
        AcmeChallengeToken::try_new("challenge-token").expect("token"),
        AcmeChallengeValue::try_new("challenge-token.account-thumbprint").expect("value"),
        AcmeChallengeTtlSeconds::try_new(900).expect("ttl"),
    )
    .expect("challenge")
}

fn endpoint_ip(ip: &str) -> std::net::IpAddr {
    ip.parse().expect("valid endpoint ip")
}

fn socket_addr(ip: &str, port: u16) -> SocketAddr {
    SocketAddr::new(endpoint_ip(ip), port)
}

fn source_unavailable() -> GatewayProjectionError {
    GatewayProjectionError::SourceUnavailable {
        message: "kv read timed out".to_owned(),
    }
}

fn current_state(projection: GatewayProjection) -> GatewayProjectionState {
    GatewayProjectionState {
        last_good: Some(projection),
        last_error: None,
    }
}

fn failed_state(
    projection: GatewayProjection,
    error: GatewayProjectionError,
) -> GatewayProjectionState {
    GatewayProjectionState {
        last_good: Some(projection),
        last_error: Some(error),
    }
}

fn running() -> ContainerRuntimeState {
    ContainerRuntimeState::running_at(endpoint_ip("10.0.0.1"))
}

fn exited() -> ContainerRuntimeState {
    ContainerRuntimeState::Exited
}
