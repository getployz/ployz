use std::net::Ipv4Addr;

#[cfg(test)]
use ployz_types::model::MachineTopology;
use ployz_types::spec::Namespace;

use crate::snapshot::DnsSnapshot;

// ---------------------------------------------------------------------------
// DnsQuery — parsed query classification
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsQuery {
    /// "db" or "db.ployz.internal" — needs namespace from source IP
    ServiceImplicit { service: String },
    /// "db.prod.ployz.internal" — namespace explicit in query
    ServiceExplicit {
        service: String,
        namespace: Namespace,
    },
    /// "_services.ployz.internal" — TXT, needs namespace from source IP
    ListServicesImplicit,
    /// "_services.prod.ployz.internal" — TXT, namespace explicit
    ListServicesExplicit { namespace: Namespace },
    /// "_instances.ployz.internal" — TXT, needs namespace from source IP
    ListInstancesImplicit,
    /// "_instances.api.ployz.internal" — TXT, service filter in caller namespace.
    ListInstancesServiceImplicit { service: String },
    /// "_instances.ns.prod.ployz.internal" — TXT, namespace explicit.
    ListInstancesNamespaceExplicit { namespace: Namespace },
    /// "_instances.api.ns.prod.ployz.internal" — TXT, service and namespace explicit.
    ListInstancesExplicit {
        service: String,
        namespace: Namespace,
    },
    /// "inst-1.instance.api.ployz.internal" — A, needs namespace from source IP
    InstanceImplicit {
        instance_id: String,
        service: String,
    },
    /// "inst-1.instance.api.prod.ployz.internal" — A, namespace explicit
    InstanceExplicit {
        instance_id: String,
        service: String,
        namespace: Namespace,
    },
    /// Anything else
    Unknown,
}

// ---------------------------------------------------------------------------
// ResolveResult
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveResult {
    /// A records — overlay IPs for the service
    Addresses(Vec<Ipv4Addr>),
    /// TXT records — list of service names in namespace
    ServiceList(Vec<String>),
    /// TXT records — instance diagnostics
    InstanceList(Vec<String>),
    /// Domain does not exist
    NxDomain,
}

// ---------------------------------------------------------------------------
// Query parsing
// ---------------------------------------------------------------------------

/// Parse a DNS query name into a structured `DnsQuery`.
///
/// Handles:
///   `db`                          → ServiceImplicit
///   `db.ployz.internal`           → ServiceImplicit
///   `db.prod.ployz.internal`      → ServiceExplicit
///   `_services.ployz.internal`    → ListServicesImplicit
///   `_services.prod.ployz.internal` → ListServicesExplicit
///   `_instances.ployz.internal`   → ListInstancesImplicit
///   `_instances.api.ployz.internal` → ListInstancesServiceImplicit
///   `_instances.ns.prod.ployz.internal` → ListInstancesNamespaceExplicit
///   `_instances.api.ns.prod.ployz.internal` → ListInstancesExplicit
///   `inst-1.instance.api.ployz.internal` → InstanceImplicit
///   `inst-1.instance.api.prod.ployz.internal` → InstanceExplicit
///   everything else               → Unknown
#[must_use]
pub fn parse_query(name: &str) -> DnsQuery {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    let labels: Vec<&str> = name.split('.').collect();

    match labels.as_slice() {
        // bare name: "db"
        [service] if !service.is_empty() => DnsQuery::ServiceImplicit {
            service: (*service).to_string(),
        },

        // "service.ployz.internal"
        [service, "ployz", "internal"] if !service.is_empty() => {
            if *service == "_services" {
                DnsQuery::ListServicesImplicit
            } else if *service == "_instances" {
                DnsQuery::ListInstancesImplicit
            } else {
                DnsQuery::ServiceImplicit {
                    service: (*service).to_string(),
                }
            }
        }

        // "service.namespace.ployz.internal"
        [service, namespace, "ployz", "internal"]
            if !service.is_empty() && !namespace.is_empty() =>
        {
            if *service == "_services" {
                DnsQuery::ListServicesExplicit {
                    namespace: Namespace::new((*namespace).to_string()),
                }
            } else if *service == "_instances" {
                DnsQuery::ListInstancesServiceImplicit {
                    service: (*namespace).to_string(),
                }
            } else {
                DnsQuery::ServiceExplicit {
                    service: (*service).to_string(),
                    namespace: Namespace::new((*namespace).to_string()),
                }
            }
        }

        // "_instances.ns.namespace.ployz.internal"
        ["_instances", "ns", namespace, "ployz", "internal"] if !namespace.is_empty() => {
            DnsQuery::ListInstancesNamespaceExplicit {
                namespace: Namespace::new((*namespace).to_string()),
            }
        }

        // "_instances.service.ns.namespace.ployz.internal"
        ["_instances", service, "ns", namespace, "ployz", "internal"]
            if !service.is_empty() && !namespace.is_empty() =>
        {
            DnsQuery::ListInstancesExplicit {
                service: (*service).to_string(),
                namespace: Namespace::new((*namespace).to_string()),
            }
        }

        // "instance-id.instance.service.ployz.internal"
        [instance_id, "instance", service, "ployz", "internal"]
            if !instance_id.is_empty() && !service.is_empty() =>
        {
            DnsQuery::InstanceImplicit {
                instance_id: (*instance_id).to_string(),
                service: (*service).to_string(),
            }
        }

        // "instance-id.instance.service.namespace.ployz.internal"
        [
            instance_id,
            "instance",
            service,
            namespace,
            "ployz",
            "internal",
        ] if !instance_id.is_empty() && !service.is_empty() && !namespace.is_empty() => {
            DnsQuery::InstanceExplicit {
                instance_id: (*instance_id).to_string(),
                service: (*service).to_string(),
                namespace: Namespace::new((*namespace).to_string()),
            }
        }

        // Everything else (including "db.internal", "google.com", etc.)
        _ => DnsQuery::Unknown,
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Resolve a parsed DNS query against the snapshot.
///
/// `caller_namespace` is derived from the source IP of the query. If `None`,
/// only explicit-namespace queries can be resolved.
#[must_use]
pub fn resolve(
    snapshot: &DnsSnapshot,
    query: DnsQuery,
    caller_namespace: Option<&Namespace>,
) -> ResolveResult {
    match query {
        DnsQuery::ServiceImplicit { service } => {
            let Some(ns) = caller_namespace else {
                return ResolveResult::NxDomain;
            };
            lookup_service(snapshot, ns, &service)
        }
        DnsQuery::ServiceExplicit { service, namespace } => {
            lookup_service(snapshot, &namespace, &service)
        }
        DnsQuery::ListServicesImplicit => {
            let Some(ns) = caller_namespace else {
                return ResolveResult::NxDomain;
            };
            lookup_service_list(snapshot, ns)
        }
        DnsQuery::ListServicesExplicit { namespace } => lookup_service_list(snapshot, &namespace),
        DnsQuery::ListInstancesImplicit => {
            let Some(ns) = caller_namespace else {
                return ResolveResult::NxDomain;
            };
            lookup_instance_list(snapshot, ns, None)
        }
        DnsQuery::ListInstancesServiceImplicit { service } => {
            let Some(ns) = caller_namespace else {
                return ResolveResult::NxDomain;
            };
            lookup_instance_list(snapshot, ns, Some(&service))
        }
        DnsQuery::ListInstancesNamespaceExplicit { namespace } => {
            lookup_instance_list(snapshot, &namespace, None)
        }
        DnsQuery::ListInstancesExplicit { service, namespace } => {
            lookup_instance_list(snapshot, &namespace, Some(&service))
        }
        DnsQuery::InstanceImplicit {
            instance_id,
            service,
        } => {
            let Some(ns) = caller_namespace else {
                return ResolveResult::NxDomain;
            };
            lookup_instance(snapshot, ns, &service, &instance_id)
        }
        DnsQuery::InstanceExplicit {
            instance_id,
            service,
            namespace,
        } => lookup_instance(snapshot, &namespace, &service, &instance_id),
        DnsQuery::Unknown => ResolveResult::NxDomain,
    }
}

fn lookup_service(snapshot: &DnsSnapshot, namespace: &Namespace, service: &str) -> ResolveResult {
    match snapshot.lookup_service(namespace, service) {
        Some(ips) if !ips.is_empty() => ResolveResult::Addresses(ips.to_vec()),
        _ => ResolveResult::NxDomain,
    }
}

fn lookup_instance(
    snapshot: &DnsSnapshot,
    namespace: &Namespace,
    service: &str,
    instance_id: &str,
) -> ResolveResult {
    match snapshot.lookup_instance(namespace, service, instance_id) {
        Some(ip) => ResolveResult::Addresses(vec![ip]),
        None => ResolveResult::NxDomain,
    }
}

fn lookup_instance_list(
    snapshot: &DnsSnapshot,
    namespace: &Namespace,
    service: Option<&str>,
) -> ResolveResult {
    let records = snapshot.instance_txt_records(namespace, service);
    if records.is_empty() {
        ResolveResult::NxDomain
    } else {
        ResolveResult::InstanceList(records)
    }
}

fn lookup_service_list(snapshot: &DnsSnapshot, namespace: &Namespace) -> ResolveResult {
    match snapshot.service_names.get(namespace) {
        Some(names) if !names.is_empty() => ResolveResult::ServiceList(names.clone()),
        _ => ResolveResult::NxDomain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // parse_query tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_bare_name() {
        assert_eq!(
            parse_query("db"),
            DnsQuery::ServiceImplicit {
                service: "db".into()
            }
        );
    }

    #[test]
    fn parse_bare_name_trailing_dot() {
        assert_eq!(
            parse_query("db."),
            DnsQuery::ServiceImplicit {
                service: "db".into()
            }
        );
    }

    #[test]
    fn parse_service_ployz_internal() {
        assert_eq!(
            parse_query("db.ployz.internal"),
            DnsQuery::ServiceImplicit {
                service: "db".into()
            }
        );
    }

    #[test]
    fn parse_service_explicit_namespace() {
        assert_eq!(
            parse_query("db.prod.ployz.internal"),
            DnsQuery::ServiceExplicit {
                service: "db".into(),
                namespace: Namespace::new("prod"),
            }
        );
    }

    #[test]
    fn parse_list_services_implicit() {
        assert_eq!(
            parse_query("_services.ployz.internal"),
            DnsQuery::ListServicesImplicit,
        );
    }

    #[test]
    fn parse_list_services_explicit() {
        assert_eq!(
            parse_query("_services.prod.ployz.internal"),
            DnsQuery::ListServicesExplicit {
                namespace: Namespace::new("prod"),
            },
        );
    }

    #[test]
    fn parse_list_instances_implicit() {
        assert_eq!(
            parse_query("_instances.ployz.internal"),
            DnsQuery::ListInstancesImplicit,
        );
    }

    #[test]
    fn parse_list_instances_service_implicit() {
        assert_eq!(
            parse_query("_instances.api.ployz.internal"),
            DnsQuery::ListInstancesServiceImplicit {
                service: "api".into(),
            },
        );
    }

    #[test]
    fn parse_list_instances_namespace_explicit() {
        assert_eq!(
            parse_query("_instances.ns.prod.ployz.internal"),
            DnsQuery::ListInstancesNamespaceExplicit {
                namespace: Namespace::new("prod"),
            },
        );
    }

    #[test]
    fn parse_list_instances_explicit_service_and_namespace() {
        assert_eq!(
            parse_query("_instances.api.ns.prod.ployz.internal"),
            DnsQuery::ListInstancesExplicit {
                service: "api".into(),
                namespace: Namespace::new("prod"),
            },
        );
    }

    #[test]
    fn parse_instance_implicit() {
        assert_eq!(
            parse_query("inst-1.instance.api.ployz.internal"),
            DnsQuery::InstanceImplicit {
                instance_id: "inst-1".into(),
                service: "api".into(),
            },
        );
    }

    #[test]
    fn parse_instance_explicit() {
        assert_eq!(
            parse_query("inst-1.instance.api.prod.ployz.internal"),
            DnsQuery::InstanceExplicit {
                instance_id: "inst-1".into(),
                service: "api".into(),
                namespace: Namespace::new("prod"),
            },
        );
    }

    #[test]
    fn parse_not_our_domain() {
        assert_eq!(parse_query("db.internal"), DnsQuery::Unknown);
        assert_eq!(parse_query("google.com"), DnsQuery::Unknown);
        assert_eq!(parse_query("a.b.c.d.ployz.internal"), DnsQuery::Unknown);
    }

    #[test]
    fn parse_case_insensitive() {
        assert_eq!(
            parse_query("DB.Prod.Ployz.Internal"),
            DnsQuery::ServiceExplicit {
                service: "db".into(),
                namespace: Namespace::new("prod"),
            }
        );
    }

    // -----------------------------------------------------------------------
    // resolve tests
    // -----------------------------------------------------------------------

    fn insert_service(
        snapshot: &mut crate::snapshot::DnsSnapshot,
        namespace: &str,
        service: &str,
        ips: Vec<std::net::Ipv4Addr>,
    ) {
        snapshot
            .services
            .entry(Namespace::new(namespace))
            .or_default()
            .insert(service.into(), ips);
    }

    fn insert_instance(
        snapshot: &mut crate::snapshot::DnsSnapshot,
        namespace: &str,
        service: &str,
        instance_id: &str,
        ip: std::net::Ipv4Addr,
    ) {
        snapshot
            .instances
            .entry(Namespace::new(namespace))
            .or_default()
            .push(crate::snapshot::DnsInstanceDiagnostic {
                service: service.into(),
                instance_id: instance_id.into(),
                machine_id: "machine-1".into(),
                topology: MachineTopology::local(),
                slot_id: "slot-1".into(),
                overlay_ip: ip,
            });
        insert_service(snapshot, namespace, service, vec![ip]);
    }

    #[test]
    fn resolve_explicit_found() {
        let mut snapshot = crate::snapshot::DnsSnapshot::empty();
        let ip = std::net::Ipv4Addr::new(10, 42, 1, 10);
        insert_service(&mut snapshot, "prod", "db", vec![ip]);

        let result = resolve(
            &snapshot,
            DnsQuery::ServiceExplicit {
                service: "db".into(),
                namespace: Namespace::new("prod"),
            },
            None,
        );
        assert_eq!(result, ResolveResult::Addresses(vec![ip]));
    }

    #[test]
    fn resolve_implicit_no_namespace() {
        let snapshot = crate::snapshot::DnsSnapshot::empty();
        let result = resolve(
            &snapshot,
            DnsQuery::ServiceImplicit {
                service: "db".into(),
            },
            None,
        );
        assert_eq!(result, ResolveResult::NxDomain);
    }

    #[test]
    fn resolve_implicit_with_namespace() {
        let mut snapshot = crate::snapshot::DnsSnapshot::empty();
        let ip = std::net::Ipv4Addr::new(10, 42, 1, 10);
        let ns = Namespace::new("prod");
        insert_service(&mut snapshot, "prod", "db", vec![ip]);

        let result = resolve(
            &snapshot,
            DnsQuery::ServiceImplicit {
                service: "db".into(),
            },
            Some(&ns),
        );
        assert_eq!(result, ResolveResult::Addresses(vec![ip]));
    }

    #[test]
    fn resolve_unknown_is_nxdomain() {
        let snapshot = crate::snapshot::DnsSnapshot::empty();
        let result = resolve(&snapshot, DnsQuery::Unknown, None);
        assert_eq!(result, ResolveResult::NxDomain);
    }

    #[test]
    fn resolve_instances_implicit_with_namespace() {
        let mut snapshot = crate::snapshot::DnsSnapshot::empty();
        let ns = Namespace::new("prod");
        insert_instance(
            &mut snapshot,
            "prod",
            "api",
            "inst-1",
            std::net::Ipv4Addr::new(10, 42, 1, 10),
        );

        let result = resolve(&snapshot, DnsQuery::ListInstancesImplicit, Some(&ns));

        assert_eq!(
            result,
            ResolveResult::InstanceList(vec![
                "service=api,instance=inst-1,machine=machine-1,region=local,az=none,slot=slot-1,ip=10.42.1.10".into()
            ])
        );
    }

    #[test]
    fn resolve_instances_service_implicit_filters_caller_namespace() {
        let mut snapshot = crate::snapshot::DnsSnapshot::empty();
        let ns = Namespace::new("prod");
        insert_instance(
            &mut snapshot,
            "prod",
            "api",
            "inst-1",
            std::net::Ipv4Addr::new(10, 42, 1, 10),
        );
        insert_instance(
            &mut snapshot,
            "api",
            "other",
            "inst-2",
            std::net::Ipv4Addr::new(10, 42, 1, 11),
        );

        let result = resolve(
            &snapshot,
            DnsQuery::ListInstancesServiceImplicit {
                service: "api".into(),
            },
            Some(&ns),
        );

        assert_eq!(
            result,
            ResolveResult::InstanceList(vec![
                "service=api,instance=inst-1,machine=machine-1,region=local,az=none,slot=slot-1,ip=10.42.1.10".into()
            ])
        );
    }

    #[test]
    fn resolve_instances_namespace_explicit_lists_namespace() {
        let mut snapshot = crate::snapshot::DnsSnapshot::empty();
        insert_instance(
            &mut snapshot,
            "prod",
            "api",
            "inst-1",
            std::net::Ipv4Addr::new(10, 42, 1, 10),
        );

        let result = resolve(
            &snapshot,
            DnsQuery::ListInstancesNamespaceExplicit {
                namespace: Namespace::new("prod"),
            },
            None,
        );

        assert_eq!(
            result,
            ResolveResult::InstanceList(vec![
                "service=api,instance=inst-1,machine=machine-1,region=local,az=none,slot=slot-1,ip=10.42.1.10".into()
            ])
        );
    }

    #[test]
    fn resolve_instance_explicit_found() {
        let mut snapshot = crate::snapshot::DnsSnapshot::empty();
        let ip = std::net::Ipv4Addr::new(10, 42, 1, 10);
        insert_instance(&mut snapshot, "prod", "api", "inst-1", ip);

        let result = resolve(
            &snapshot,
            DnsQuery::InstanceExplicit {
                instance_id: "inst-1".into(),
                service: "api".into(),
                namespace: Namespace::new("prod"),
            },
            None,
        );

        assert_eq!(result, ResolveResult::Addresses(vec![ip]));
    }

    #[test]
    fn resolve_instance_wrong_service_is_nxdomain() {
        let mut snapshot = crate::snapshot::DnsSnapshot::empty();
        insert_instance(
            &mut snapshot,
            "prod",
            "api",
            "inst-1",
            std::net::Ipv4Addr::new(10, 42, 1, 10),
        );

        let result = resolve(
            &snapshot,
            DnsQuery::InstanceExplicit {
                instance_id: "inst-1".into(),
                service: "worker".into(),
                namespace: Namespace::new("prod"),
            },
            None,
        );

        assert_eq!(result, ResolveResult::NxDomain);
    }
}
