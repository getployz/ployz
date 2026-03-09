use std::net::Ipv4Addr;

use ployz_sdk::spec::Namespace;

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
///   everything else               → Unknown
#[must_use]
pub fn parse_query(name: &str) -> DnsQuery {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    let labels: Vec<&str> = name.split('.').collect();

    #[allow(clippy::wildcard_enum_match_arm)]
    match labels.as_slice() {
        // bare name: "db"
        [service] if !service.is_empty() => DnsQuery::ServiceImplicit {
            service: (*service).to_string(),
        },

        // "service.ployz.internal"
        [service, "ployz", "internal"] if !service.is_empty() => {
            if *service == "_services" {
                DnsQuery::ListServicesImplicit
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
                    namespace: Namespace((*namespace).to_string()),
                }
            } else {
                DnsQuery::ServiceExplicit {
                    service: (*service).to_string(),
                    namespace: Namespace((*namespace).to_string()),
                }
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
        DnsQuery::ListServicesExplicit { namespace } => {
            lookup_service_list(snapshot, &namespace)
        }
        DnsQuery::Unknown => ResolveResult::NxDomain,
    }
}

fn lookup_service(snapshot: &DnsSnapshot, namespace: &Namespace, service: &str) -> ResolveResult {
    let key = (namespace.clone(), service.to_string());
    match snapshot.services.get(&key) {
        Some(ips) if !ips.is_empty() => ResolveResult::Addresses(ips.clone()),
        _ => ResolveResult::NxDomain,
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
                namespace: Namespace("prod".into()),
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
                namespace: Namespace("prod".into()),
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
                namespace: Namespace("prod".into()),
            }
        );
    }

    // -----------------------------------------------------------------------
    // resolve tests
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_explicit_found() {
        let mut snapshot = crate::snapshot::DnsSnapshot::empty();
        let ip = std::net::Ipv4Addr::new(10, 42, 1, 10);
        snapshot
            .services
            .insert((Namespace("prod".into()), "db".into()), vec![ip]);

        let result = resolve(
            &snapshot,
            DnsQuery::ServiceExplicit {
                service: "db".into(),
                namespace: Namespace("prod".into()),
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
        let ns = Namespace("prod".into());
        snapshot
            .services
            .insert((ns.clone(), "db".into()), vec![ip]);

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
}
