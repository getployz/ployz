//! Parse an Internal DNS name into a typed query.

use hickory_server::proto::rr::Name;
use ployz_core::domain::private_dns::Fqdn;
use ployz_core::{ClusterName, MachineId, Namespace, QualifiedService, ServiceId, ServiceName};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Query {
    Forward,
    Internal(InternalQuery),
    NxDomain,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum InternalQuery {
    Empty,
    Service(QualifiedService),
    CallerService(ServiceName),
    Nearest(QualifiedService),
    ServiceId(ServiceId),
    Machine(MachineServiceTarget),
    Regional,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct MachineServiceTarget {
    pub machine_id: MachineId,
    pub identity: QualifiedService,
}

#[must_use]
pub(super) fn parse(name: &Name, cluster: Option<&ClusterName>) -> Query {
    parse_fqdn(&name.to_utf8().to_ascii_lowercase(), cluster)
}

fn parse_fqdn(fqdn: &str, cluster: Option<&ClusterName>) -> Query {
    match Fqdn::parse(fqdn) {
        Fqdn::Foreign => Query::Forward,
        Fqdn::Under {
            selector,
            alias: None,
        } => Query::Internal(parse_internal(selector)),
        Fqdn::Under {
            selector,
            alias: Some(label),
        } if cluster.is_some_and(|cluster| cluster.as_str() == label) => {
            Query::Internal(parse_internal(selector))
        }
        Fqdn::Under { alias: Some(_), .. } => Query::NxDomain,
    }
}

fn parse_internal(selector: &str) -> InternalQuery {
    let mut labels = selector.split('.');
    let labels = (
        labels.next(),
        labels.next(),
        labels.next(),
        labels.next(),
        labels.next(),
    );
    match labels {
        (Some(service), None, None, None, None) => ServiceName::parse(service)
            .map(InternalQuery::CallerService)
            .unwrap_or(InternalQuery::Empty),
        (Some(service), Some(namespace), None, None, None) => {
            identity(service, namespace).map_or(InternalQuery::Empty, InternalQuery::Service)
        }
        (Some(service), Some(namespace), Some("nearest"), None, None) => {
            identity(service, namespace).map_or(InternalQuery::Empty, InternalQuery::Nearest)
        }
        (Some(service_id), Some("id"), Some("lookup"), None, None) => {
            ServiceId::parse(service_id).map_or(InternalQuery::Empty, InternalQuery::ServiceId)
        }
        (Some(service), Some(namespace), Some(machine_id), Some("machine"), None) => {
            match (MachineId::parse(machine_id), identity(service, namespace)) {
                (Ok(machine_id), Some(identity)) => InternalQuery::Machine(MachineServiceTarget {
                    machine_id,
                    identity,
                }),
                _ => InternalQuery::Empty,
            }
        }
        (Some(service), Some(namespace), Some(region), Some("region"), None)
            if !region.is_empty() && identity(service, namespace).is_some() =>
        {
            InternalQuery::Regional
        }
        _ => InternalQuery::Empty,
    }
}

fn identity(service: &str, namespace: &str) -> Option<QualifiedService> {
    Some(QualifiedService::new(
        Namespace::parse(namespace).ok()?,
        ServiceName::parse(service).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_label_internal_name_is_caller_service() {
        assert_eq!(
            query("web.internal."),
            internal(InternalQuery::CallerService(
                ServiceName::parse("web").unwrap()
            ))
        );
    }

    #[test]
    fn forwards_names_outside_the_internal_zone() {
        assert_eq!(query("example.com."), Query::Forward);
        assert_eq!(query("api.internal.com."), Query::Forward);
        assert_eq!(query("internal.example."), Query::Forward);
    }

    #[test]
    fn parses_every_canonical_form() {
        let service_id = ServiceId::parse("b".repeat(32)).unwrap();
        let machine_id = MachineId::parse("a".repeat(32)).unwrap();
        let identity = QualifiedService::parse("shop-staging/web").unwrap();

        assert_eq!(
            query("web.shop-staging.internal."),
            internal(InternalQuery::Service(identity.clone()))
        );
        assert_eq!(
            query("web.shop-staging.nearest.internal."),
            internal(InternalQuery::Nearest(identity.clone()))
        );
        assert_eq!(
            query(&format!("{service_id}.id.lookup.internal.")),
            internal(InternalQuery::ServiceId(service_id))
        );
        assert_eq!(
            query(&format!("web.shop-staging.{machine_id}.machine.internal.")),
            internal(InternalQuery::Machine(MachineServiceTarget {
                machine_id,
                identity,
            }))
        );
        assert_eq!(
            query("web.shop-staging.eu-west.region.internal."),
            internal(InternalQuery::Regional)
        );
    }

    #[test]
    fn reserved_words_remain_valid_service_and_namespaces() {
        for name in ["rr", "nearest", "machine", "region", "id", "lookup"] {
            assert_eq!(
                query(&format!("{name}.namespace.internal.")),
                internal(InternalQuery::Service(
                    QualifiedService::parse(format!("namespace/{name}")).unwrap()
                ))
            );
            assert_eq!(
                query(&format!("service.{name}.internal.")),
                internal(InternalQuery::Service(
                    QualifiedService::parse(format!("{name}/service")).unwrap()
                ))
            );
        }
    }

    #[test]
    fn hex_service_name_is_not_a_service_id() {
        let name = "b".repeat(32);
        assert_eq!(
            query(&format!("{name}.shop.internal.")),
            internal(InternalQuery::Service(
                QualifiedService::parse(format!("shop/{name}")).unwrap()
            ))
        );
        assert_eq!(
            query(&format!("{name}.internal.")),
            internal(InternalQuery::CallerService(
                ServiceName::parse(name).unwrap()
            ))
        );
    }

    #[test]
    fn malformed_internal_names_do_not_fall_back() {
        let machine_id = "a".repeat(32);
        for name in [
            "internal.".to_owned(),
            "rr.web.shop.internal.".to_owned(),
            "web.shop.lookup.internal.".to_owned(),
            "web.shop.nearest.extra.internal.".to_owned(),
            "not-an-id.id.lookup.internal.".to_owned(),
            "web.shop.not-an-id.machine.internal.".to_owned(),
            format!("web.{machine_id}.machine.internal."),
            format!("web.shop.{machine_id}.m.internal."),
            "web.shop.region.internal.".to_owned(),
        ] {
            assert_eq!(query(&name), internal(InternalQuery::Empty), "{name}");
        }
    }

    #[test]
    fn the_cluster_alias_reads_as_the_name_without_it() {
        let acme = ClusterName::parse("acme").unwrap();
        for (alias, short) in [
            (
                "postgres.prod.acme.ployz.internal.",
                "postgres.prod.internal.",
            ),
            ("postgres.acme.ployz.internal.", "postgres.internal."),
            (
                "Postgres.Prod.ACME.ployz.internal.",
                "postgres.prod.internal.",
            ),
            (
                "web.shop.nearest.acme.ployz.internal.",
                "web.shop.nearest.internal.",
            ),
            ("acme.ployz.internal.", "internal."),
        ] {
            assert_eq!(in_cluster(alias, Some(&acme)), query(short), "{alias}");
        }
    }

    #[test]
    fn a_foreign_or_unset_cluster_label_is_nxdomain() {
        let acme = ClusterName::parse("acme").unwrap();
        for name in [
            "postgres.prod.other.ployz.internal.",
            "other.ployz.internal.",
            "postgres.prod.acme.ployz.ployz.internal.",
        ] {
            assert_eq!(in_cluster(name, Some(&acme)), Query::NxDomain, "{name}");
        }
        assert_eq!(
            in_cluster("postgres.prod.acme.ployz.internal.", None),
            Query::NxDomain
        );
    }

    #[test]
    fn short_forms_ignore_the_cluster_name() {
        let acme = ClusterName::parse("acme").unwrap();
        for name in [
            "postgres.prod.internal.",
            "postgres.internal.",
            "ployz.internal.",
            "web.acme.internal.",
        ] {
            assert_eq!(in_cluster(name, Some(&acme)), query(name), "{name}");
        }
        assert_eq!(
            query("ployz.internal."),
            internal(InternalQuery::CallerService(
                ServiceName::parse("ployz").unwrap()
            ))
        );
    }

    fn query(name: &str) -> Query {
        in_cluster(name, None)
    }

    fn in_cluster(name: &str, cluster: Option<&ClusterName>) -> Query {
        parse(&Name::from_ascii(name).unwrap(), cluster)
    }

    fn internal(query: InternalQuery) -> Query {
        Query::Internal(query)
    }
}
