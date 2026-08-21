//! Parse an Internal DNS name into a typed query.

use hickory_server::proto::rr::Name;
use ployz_core::{MachineId, ProjectName, QualifiedService, ServiceId, ServiceName};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Query {
    Forward,
    Internal(InternalQuery),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum InternalQuery {
    Empty,
    Service(QualifiedService),
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
pub(super) fn parse(name: &Name) -> Query {
    parse_fqdn(&name.to_utf8().to_ascii_lowercase())
}

fn parse_fqdn(fqdn: &str) -> Query {
    let selector = if fqdn == "internal." {
        ""
    } else if let Some(selector) = fqdn.strip_suffix(".internal.") {
        selector
    } else {
        return Query::Forward;
    };
    Query::Internal(parse_internal(selector))
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
        (Some(service), Some(project), None, None, None) => {
            identity(service, project).map_or(InternalQuery::Empty, InternalQuery::Service)
        }
        (Some(service), Some(project), Some("nearest"), None, None) => {
            identity(service, project).map_or(InternalQuery::Empty, InternalQuery::Nearest)
        }
        (Some(service_id), Some("id"), Some("lookup"), None, None) => {
            ServiceId::parse(service_id).map_or(InternalQuery::Empty, InternalQuery::ServiceId)
        }
        (Some(service), Some(project), Some(machine_id), Some("machine"), None) => {
            match (MachineId::parse(machine_id), identity(service, project)) {
                (Ok(machine_id), Some(identity)) => InternalQuery::Machine(MachineServiceTarget {
                    machine_id,
                    identity,
                }),
                _ => InternalQuery::Empty,
            }
        }
        (Some(service), Some(project), Some(region), Some("region"), None)
            if !region.is_empty() && identity(service, project).is_some() =>
        {
            InternalQuery::Regional
        }
        _ => InternalQuery::Empty,
    }
}

fn identity(service: &str, project: &str) -> Option<QualifiedService> {
    Some(QualifiedService::new(
        ProjectName::parse(project).ok()?,
        ServiceName::parse(service).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn reserved_words_remain_valid_service_and_project_names() {
        for name in ["rr", "nearest", "machine", "region", "id", "lookup"] {
            assert_eq!(
                query(&format!("{name}.project.internal.")),
                internal(InternalQuery::Service(
                    QualifiedService::parse(format!("project/{name}")).unwrap()
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
            internal(InternalQuery::Empty)
        );
    }

    #[test]
    fn malformed_internal_names_do_not_fall_back() {
        let machine_id = "a".repeat(32);
        for name in [
            "internal.".to_owned(),
            "web.internal.".to_owned(),
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

    fn query(name: &str) -> Query {
        parse(&Name::from_ascii(name).unwrap())
    }

    fn internal(query: InternalQuery) -> Query {
        Query::Internal(query)
    }
}
