//! Observer-derived Hostname Owner for an explicit Ingress Hostname.

use std::collections::BTreeMap;

use super::{ContainerKind, ContainerObservation, PortPublication};
use crate::{IngressHost, QualifiedService, ServiceId};

/// Hostname Owner for each explicit Ingress Hostname in these observations.
///
/// Derived from Service Container observations that already replicate. Not a
/// persisted record. Different observation sets may select different owners;
/// the same observations always select the same owner.
#[must_use]
pub fn hostname_owners<'a>(
    observations: impl IntoIterator<Item = &'a ContainerObservation>,
) -> BTreeMap<IngressHost, QualifiedService> {
    let mut winners: BTreeMap<IngressHost, (i64, QualifiedService, ServiceId)> = BTreeMap::new();
    for observation in observations {
        if observation.kind != ContainerKind::ServiceContainer {
            continue;
        }
        let created_at = observation.created_at_unix_nanos;
        let service_id = observation.service_id();
        let identity = observation.identity();
        for hostname in explicit_ingress_hosts(&observation.resolved_spec.ports) {
            let better = match winners.get(hostname) {
                Some((best_at, best_id, best_sid)) => {
                    (created_at, &identity, service_id) < (*best_at, best_id, *best_sid)
                }
                None => true,
            };
            if better {
                winners.insert(hostname.clone(), (created_at, identity.clone(), service_id));
            }
        }
    }
    winners
        .into_iter()
        .map(|(hostname, (_, identity, _))| (hostname, identity))
        .collect()
}

/// Explicit Ingress Hostnames published by these ports.
pub fn explicit_ingress_hosts(ports: &[PortPublication]) -> impl Iterator<Item = &IngressHost> {
    ports.iter().filter_map(|port| match port {
        PortPublication::Ingress { hostname, .. } => hostname.as_explicit_host(),
        PortPublication::Host { .. } => None,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::num::NonZeroU16;

    use serde_json::json;

    use super::hostname_owners;
    use crate::{
        ContainerId, ContainerKind, ContainerObservation, ContainerRuntimeObservation,
        HttpProtocol, IngressHost, IngressHostname, MachineId, PortPublication, ProjectName,
        QualifiedService, ResolvedServiceSpec, ServiceId, ServiceName,
    };

    #[test]
    fn oldest_container_timestamp_wins_before_qualified_name() {
        let shop = observation("shop", "web", '1', 'a', 10, "api.example.com");
        let blog = observation("blog", "web", '2', 'b', 5, "api.example.com");
        assert_eq!(
            hostname_owners([&shop, &blog]).get(&host("api.example.com")),
            Some(&QualifiedService::parse("blog/web").unwrap())
        );
    }

    #[test]
    fn equal_timestamps_break_ties_on_qualified_service_then_service_id() {
        let hostname = host("api.example.com");
        let shop_web = observation("shop", "web", '1', 'a', 7, hostname.as_str());
        let blog_web = observation("blog", "web", '9', 'b', 7, hostname.as_str());
        assert_eq!(
            hostname_owners([&shop_web, &blog_web]).get(&hostname),
            Some(&QualifiedService::parse("blog/web").unwrap())
        );

        let shop_web = observation("shop", "web", '9', 'c', 7, hostname.as_str());
        let shop_api = observation("shop", "api", '1', 'd', 7, hostname.as_str());
        assert_eq!(
            hostname_owners([&shop_web, &shop_api]).get(&hostname),
            Some(&QualifiedService::parse("shop/api").unwrap())
        );
    }

    #[test]
    fn same_observations_select_the_same_owner_and_partial_views_may_disagree() {
        let hostname = host("api.example.com");
        let shop = observation("shop", "web", '1', 'a', 1, hostname.as_str());
        let blog = observation("blog", "web", '2', 'b', 2, hostname.as_str());
        let shop_only = hostname_owners([&shop]);
        let blog_only = hostname_owners([&blog]);
        assert_eq!(
            shop_only.get(&hostname),
            Some(&QualifiedService::parse("shop/web").unwrap())
        );
        assert_eq!(
            blog_only.get(&hostname),
            Some(&QualifiedService::parse("blog/web").unwrap())
        );
        assert_ne!(shop_only, blog_only);

        let together = hostname_owners([&blog, &shop]);
        let together_again = hostname_owners([&shop, &blog]);
        assert_eq!(together, together_again);
        assert_eq!(
            together.get(&hostname),
            Some(&QualifiedService::parse("shop/web").unwrap())
        );
    }

    #[test]
    fn winning_qualified_service_keeps_every_claiming_container() {
        let old = observation("shop", "web", '1', 'a', 1, "api.example.com");
        let rolling = observation("shop", "web", '3', 'c', 3, "api.example.com");
        let other = observation("blog", "web", '2', 'b', 2, "api.example.com");
        assert_eq!(
            hostname_owners([&old, &rolling, &other]).get(&host("api.example.com")),
            Some(&QualifiedService::parse("shop/web").unwrap())
        );
    }

    #[test]
    fn hooks_and_unassigned_ingress_do_not_claim_a_hostname() {
        let mut hook = observation("shop", "web", '1', 'a', 1, "api.example.com");
        hook.try_update(|parts| parts.kind = ContainerKind::PreDeployHook)
            .unwrap();
        let mut assigned = observation("blog", "web", '2', 'b', 2, "api.example.com");
        assigned
            .try_update(|parts| {
                parts.resolved_spec.ports = vec![PortPublication::Ingress {
                    hostname: IngressHostname::cluster_domain(),
                    load_balancer_port: NonZeroU16::new(80).unwrap(),
                    container_port: NonZeroU16::new(80).unwrap(),
                    http_protocol: HttpProtocol::Http,
                }]
            })
            .unwrap();
        let mut chosen = observation("shop", "api", '3', 'c', 3, "api.example.com");
        chosen
            .try_update(|parts| {
                parts.resolved_spec.ports = vec![PortPublication::Ingress {
                    hostname: IngressHostname::cluster_domain_label("api").unwrap(),
                    load_balancer_port: NonZeroU16::new(80).unwrap(),
                    container_port: NonZeroU16::new(80).unwrap(),
                    http_protocol: HttpProtocol::Http,
                }]
            })
            .unwrap();
        assert!(hostname_owners([&hook, &assigned, &chosen]).is_empty());
    }

    fn host(name: &str) -> IngressHost {
        IngressHost::parse(name).unwrap()
    }

    fn observation(
        project: &str,
        name: &str,
        service_id: char,
        container: char,
        created_at: i64,
        hostname: &str,
    ) -> ContainerObservation {
        let service_id = ServiceId::parse(service_id.to_string().repeat(32)).unwrap();
        let service_name = ServiceName::parse(name).unwrap();
        let resolved_spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": service_id,
            "name": service_name,
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "example.test/image", "pull_policy": "missing" },
            "ports": [{
                "mode": "ingress",
                "hostname": { "kind": "explicit", "hostname": hostname },
                "load_balancer_port": 80,
                "container_port": 80,
                "http_protocol": "http"
            }]
        }))
        .unwrap();
        ContainerObservation::try_from(crate::ContainerObservationParts {
            container_id: ContainerId::parse(container.to_string().repeat(64)).unwrap(),
            display_name: format!("{name}-{container}"),
            created_at_unix_nanos: created_at,
            machine_id: MachineId::parse("1".repeat(32)).unwrap(),
            project_name: ProjectName::parse(project).unwrap(),
            kind: ContainerKind::ServiceContainer,
            runtime: ContainerRuntimeObservation::Created,
            effective_healthcheck: None,
            resolved_spec,
            address: None,
            labels: BTreeMap::new(),
        })
        .unwrap()
    }
}
