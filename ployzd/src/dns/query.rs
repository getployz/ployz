//! Parse an Internal DNS name into a typed query and target.

use hickory_server::proto::rr::Name;
use ployz_core::{MachineId, ServiceId, ServiceName};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Query {
    Forward,
    Internal(InternalQuery),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct InternalQuery {
    pub target: Target,
    pub nearest: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Target {
    Empty,
    ServiceId { id: ServiceId, name: ServiceName },
    ServiceName(ServiceName),
    MachineService(MachineServiceTarget),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct MachineServiceTarget {
    pub machine_id: MachineId,
    pub service_name: ServiceName,
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
    let (selector, nearest) = selector
        .strip_prefix("nearest.")
        .map_or((selector, false), |selector| (selector, true));
    let selector = selector.strip_prefix("rr.").unwrap_or(selector);
    Query::Internal(InternalQuery {
        target: parse_target(selector),
        nearest,
    })
}

fn parse_target(selector: &str) -> Target {
    if selector.is_empty() {
        return Target::Empty;
    }
    if let Some((machine, service)) = selector.split_once(".m.")
        && let Ok(machine_id) = MachineId::parse(machine)
        && let Ok(service_name) = ServiceName::parse(service)
    {
        return Target::MachineService(MachineServiceTarget {
            machine_id,
            service_name,
        });
    }
    if let Ok(id) = ServiceId::parse(selector) {
        return Target::ServiceId {
            id,
            // A Service ID is also a valid Service Name; ID index wins when present.
            name: ServiceName::parse(selector).expect("a Service ID is a DNS-label Service Name"),
        };
    }
    ServiceName::parse(selector).map_or(Target::Empty, Target::ServiceName)
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
    fn empty_internal_name_is_an_empty_target() {
        assert_eq!(query("internal."), internal(Target::Empty, false));
    }

    #[test]
    fn service_id_target_wins_the_hex_label() {
        let id = ServiceId::parse("b".repeat(32)).unwrap();
        assert_eq!(
            query(&format!("{id}.internal.")),
            internal(
                Target::ServiceId {
                    id,
                    name: ServiceName::parse(id.as_str()).unwrap(),
                },
                false,
            )
        );
    }

    #[test]
    fn service_name_target_uses_the_remaining_label() {
        assert_eq!(
            query("api.internal."),
            internal(
                Target::ServiceName(ServiceName::parse("api").unwrap()),
                false
            )
        );
    }

    #[test]
    fn machine_service_target_splits_machine_id_and_service_name() {
        let machine = MachineId::parse("a".repeat(32)).unwrap();
        let service = ServiceName::parse("api").unwrap();
        assert_eq!(
            query(&format!("{machine}.m.api.internal.")),
            internal(
                Target::MachineService(MachineServiceTarget {
                    machine_id: machine,
                    service_name: service,
                }),
                false
            )
        );
    }

    #[test]
    fn nearest_prefix_marks_the_query_without_changing_the_target() {
        let machine = MachineId::parse("a".repeat(32)).unwrap();
        assert_eq!(
            query("nearest.api.internal."),
            internal(
                Target::ServiceName(ServiceName::parse("api").unwrap()),
                true
            )
        );
        assert_eq!(
            query(&format!("nearest.{machine}.m.api.internal.")),
            internal(
                Target::MachineService(MachineServiceTarget {
                    machine_id: machine,
                    service_name: ServiceName::parse("api").unwrap(),
                }),
                true
            )
        );
    }

    #[test]
    fn rr_prefix_is_stripped_and_does_not_set_nearest() {
        assert_eq!(
            query("rr.api.internal."),
            internal(
                Target::ServiceName(ServiceName::parse("api").unwrap()),
                false
            )
        );
        assert_eq!(
            query("nearest.rr.api.internal."),
            internal(
                Target::ServiceName(ServiceName::parse("api").unwrap()),
                true
            )
        );
    }

    #[test]
    fn rr_before_nearest_is_not_a_mode() {
        assert_eq!(
            query("rr.nearest.api.internal."),
            internal(Target::Empty, false)
        );
    }

    fn query(name: &str) -> Query {
        parse(&Name::from_ascii(name).unwrap())
    }

    fn internal(target: Target, nearest: bool) -> Query {
        Query::Internal(InternalQuery { target, nearest })
    }
}
