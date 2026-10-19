//! Projection fixture shared by concrete renderer tests.

use super::{
    IngressEndpoint, IngressProjection, IngressPublication, IngressSite, ProjectedCertificate,
    PublicationTrace,
};
use crate::corrosion::{CertificateChallenge, CertificateMaterial};
use ployz_core::{
    ContainerAddress, DeploymentLogId, IngressHost, Machine, MachineId, MachineName,
    QualifiedService, WireGuardPublicKey,
};
use std::num::{NonZeroU16, NonZeroUsize};

/// Projection that exercises HTTP, HTTPS, challenge, ordered, and empty routes.
pub(crate) fn renderer_projection() -> IngressProjection {
    let machine_id = MachineId::parse("a".repeat(32)).unwrap();
    let owner = QualifiedService::parse("app/api").unwrap();
    let endpoint = |address: &str, port| IngressEndpoint {
        address: ContainerAddress(address.parse().unwrap()),
        port: NonZeroU16::new(port).unwrap(),
    };
    let trace = PublicationTrace {
        deploy: Some(DeploymentLogId::parse("dep_7").unwrap()),
        placement: [
            (machine_id, NonZeroUsize::new(2).unwrap()),
            (MachineId::parse("b".repeat(32)).unwrap(), NonZeroUsize::MIN),
        ]
        .into(),
    };
    let publication = |http, https| {
        Some(IngressPublication {
            owner: owner.clone(),
            http,
            https,
            trace: trace.clone(),
        })
    };
    IngressProjection {
        machine: Machine {
                        labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: machine_id,
            name: MachineName::parse("node-a").unwrap(),
            subnet: "10.210.1.0/24".parse().unwrap(),
            public_key: WireGuardPublicKey([1; 32]),
            public_ip: None,
            advertised_endpoints: Vec::new(),
            runtime: Default::default(),
            build_concurrency: None,
        },
        sites: vec![
            IngressSite {
                hostname: IngressHost::parse("empty.example.com").unwrap(),
                publication: publication(Some(Vec::new()), None),
                certificate: None,
            },
            IngressSite {
                hostname: IngressHost::parse("example.com").unwrap(),
                publication: publication(
                    Some(vec![
                        endpoint("10.210.1.2", 8080),
                        endpoint("10.210.2.2", 8080),
                    ]),
                    None,
                ),
                certificate: Some(ProjectedCertificate {
                    challenge: Some(CertificateChallenge::parse(
                        "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0",
                        "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                    ).unwrap()),
                    material: None,
                    last_error: None,
                }),
            },
            IngressSite {
                hostname: IngressHost::parse("secure.example.com").unwrap(),
                publication: publication(None, Some(vec![endpoint("10.210.1.3", 8443)])),
                certificate: Some(ProjectedCertificate {
                    challenge: None,
                    material: Some(CertificateMaterial::parse(
                        include_str!("../tests/fixtures/certificate-test-rsa.pem"),
                        include_str!("../tests/fixtures/certificate-test-rsa-key.pem"),
                    ).unwrap()),
                    last_error: None,
                }),
            },
        ],
    }
}

pub(crate) fn test_material() -> CertificateMaterial {
    let pair = rcgen::generate_simple_self_signed(["secure.example.com".to_owned()]).unwrap();
    CertificateMaterial::parse(pair.cert.pem(), pair.signing_key.serialize_pem()).unwrap()
}

mod rule_f {
    use std::collections::{BTreeMap, BTreeSet};

    use ployz_core::{
        ContainerAddress, ContainerId, ContainerKind, ContainerObservation,
        ContainerRuntimeObservation, CopyRotation, DeploymentLogId, HealthObservation, MachineId,
        Namespace, ProxyRoutes, QualifiedService, ResolvedServiceSpec, ServiceContainer,
        proxy_routes, service_containers,
    };
    use serde_json::json;

    use crate::ingress::route_acks;

    fn machine(id: char) -> MachineId {
        MachineId::parse(id.to_string().repeat(32)).unwrap()
    }

    fn copy(suffix: u8, machine: MachineId, rotation: CopyRotation) -> ContainerObservation {
        let resolved_spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": "a".repeat(32),
            "name": "api",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "example.test/image", "pull_policy": "missing" },
        }))
        .unwrap();
        ContainerObservation::try_from(ployz_core::ContainerObservationParts {
            rotation: Some(rotation),
            r#gen: None,
            container_id: ContainerId::parse(format!("{suffix:x}").repeat(64)).unwrap(),
            display_name: format!("api-{suffix}"),
            created_at_unix_nanos: 0,
            machine_id: machine,
            namespace: Namespace::parse("app").unwrap(),
            kind: ContainerKind::ServiceContainer,
            runtime: ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            effective_healthcheck: None,
            resolved_spec,
            address: Some(ContainerAddress([10, 210, suffix, 2].into())),
            labels: BTreeMap::new(),
        })
        .unwrap()
    }

    fn api() -> QualifiedService {
        QualifiedService::parse("app/api").unwrap()
    }

    fn held() -> CopyRotation {
        CopyRotation::Held(DeploymentLogId::parse("run").unwrap())
    }

    /// Routes after one pass from `current`, with `down` Machines Down.
    fn next(
        copies: impl IntoIterator<Item = ContainerObservation>,
        down: &[MachineId],
        current: &ProxyRoutes,
    ) -> ProxyRoutes {
        let containers: Vec<ServiceContainer> = service_containers(copies);
        proxy_routes(&containers, |machine| down.contains(machine), current)
    }

    /// The routes a proxy holds after routing `copies` once with no Machine Down.
    fn routed(copies: impl IntoIterator<Item = ContainerObservation>) -> ProxyRoutes {
        next(copies, &[], &ProxyRoutes::new())
    }

    fn ids(routes: &ProxyRoutes) -> Option<BTreeSet<u8>> {
        routes.get(&api()).map(|route| {
            route
                .iter()
                .map(|serving| serving.address().0.octets()[2])
                .collect()
        })
    }

    #[test]
    fn every_copy_out_and_none_pending_routes_nothing() {
        let current = routed([copy(1, machine('a'), CopyRotation::In)]);

        let routes = next(
            [
                copy(1, machine('a'), CopyRotation::Out),
                copy(2, machine('b'), CopyRotation::Out),
            ],
            &[],
            &current,
        );

        assert_eq!(ids(&routes), Some(BTreeSet::new()));
    }

    #[test]
    fn a_pending_copy_keeps_the_route_while_the_rest_are_out() {
        let current = routed([copy(1, machine('a'), CopyRotation::In)]);

        let routes = next(
            [
                copy(1, machine('a'), CopyRotation::Out),
                copy(2, machine('b'), CopyRotation::Pending),
            ],
            &[],
            &current,
        );

        assert_eq!(ids(&routes), Some(BTreeSet::from([1])));
    }

    #[test]
    fn an_in_copy_on_a_down_machine_keeps_the_current_route() {
        let current = routed([copy(1, machine('a'), CopyRotation::In)]);

        let routes = next(
            [copy(1, machine('a'), CopyRotation::In)],
            &[machine('a')],
            &current,
        );

        assert_eq!(ids(&routes), Some(BTreeSet::from([1])));
    }

    #[test]
    fn a_down_machine_leaves_the_route_while_another_copy_serves() {
        let current = routed([
            copy(1, machine('a'), CopyRotation::In),
            copy(2, machine('b'), CopyRotation::In),
        ]);

        let routes = next(
            [
                copy(1, machine('a'), CopyRotation::In),
                copy(2, machine('b'), CopyRotation::In),
            ],
            &[machine('a')],
            &current,
        );

        assert_eq!(ids(&routes), Some(BTreeSet::from([2])));
    }

    #[test]
    fn a_restarted_proxy_routes_in_copies_on_down_machines() {
        let routes = next(
            [copy(1, machine('a'), CopyRotation::In)],
            &[machine('a')],
            &ProxyRoutes::new(),
        );

        assert_eq!(ids(&routes), Some(BTreeSet::from([1])));
    }

    #[test]
    fn a_held_copy_is_never_a_candidate_and_does_not_count_as_entering() {
        let current = routed([copy(1, machine('a'), CopyRotation::In)]);

        let only_held = next([copy(1, machine('a'), held())], &[], &current);
        assert_eq!(ids(&only_held), Some(BTreeSet::new()));

        let beside_in = next(
            [
                copy(1, machine('a'), held()),
                copy(2, machine('b'), CopyRotation::In),
            ],
            &[],
            &current,
        );
        assert_eq!(ids(&beside_in), Some(BTreeSet::from([2])));
    }

    #[test]
    fn a_routed_copy_out_of_view_keeps_the_route() {
        let current = routed([copy(1, machine('a'), CopyRotation::In)]);

        let routes = next([copy(2, machine('b'), CopyRotation::Out)], &[], &current);

        assert_eq!(ids(&routes), Some(BTreeSet::from([1])));
    }

    #[test]
    fn a_service_with_no_observed_copy_has_no_route() {
        let current = routed([copy(1, machine('a'), CopyRotation::In)]);

        let routes = next([], &[], &current);

        assert_eq!(ids(&routes), None);
        assert!(route_acks(&routes).is_empty());
    }

    #[test]
    fn ack_lists_routed_container_set() {
        let current = routed([
            copy(1, machine('a'), CopyRotation::In),
            copy(2, machine('b'), CopyRotation::In),
        ]);
        let routes = next(
            [
                copy(1, machine('a'), CopyRotation::In),
                copy(2, machine('b'), CopyRotation::In),
                copy(3, machine('c'), CopyRotation::Pending),
            ],
            &[machine('a')],
            &current,
        );

        let acks = route_acks(&routes);

        let ack = acks.get(&api()).expect("api has an ack");
        assert_eq!(ack.deploy.as_str(), "rotation");
        assert_eq!(
            ack.containers,
            BTreeSet::from([ContainerId::parse("2".repeat(64)).unwrap()])
        );
        assert_eq!(acks.len(), 1);
    }
}
