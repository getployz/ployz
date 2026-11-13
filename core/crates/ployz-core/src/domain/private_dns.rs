//! The Private DNS name grammar: how a Service's `.internal` name is built, qualified and parsed.
//!
//! Containers resolve these names and Cloud stores them in `PLOYZ_PRIVATE_DOMAIN`, so the
//! zone and the Internal DNS alias suffix are written once, here. The resolver's own query
//! language (`nearest`, `id.lookup`, `machine`, `region`) stays with the resolver.

use crate::{Namespace, ServiceName};

const ZONE: &str = "internal";

/// A Service's short private address: its Private DNS name in the `.internal` zone.
#[must_use]
pub fn short(name: &ServiceName) -> String {
    format!("{name}.{ZONE}")
}

/// The search domain a Container on the bridge resolves one-label names under.
#[must_use]
pub fn search_domain(namespace: &Namespace) -> String {
    format!("{namespace}.{ZONE}")
}

/// The Internal DNS aliases of a Cluster name: every Service under `<cluster>.ployz.internal`.
#[must_use]
pub fn alias_wildcard(cluster: &str) -> String {
    format!("*.{cluster}.{}.{ZONE}", Namespace::ALIAS)
}

/// A live copy of an owner's short private address that names the owner's Namespace. An address
/// that is not a one-label host in the zone, or that already names a Namespace, is unchanged.
#[must_use]
pub fn qualify(address: &str, namespace: &Namespace) -> String {
    match address.strip_suffix(&format!(".{ZONE}")) {
        Some(host) if !host.contains('.') => format!("{host}.{namespace}.{ZONE}"),
        _ => address.to_owned(),
    }
}

/// A lowercase, trailing-dot DNS name split by the Private DNS grammar.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fqdn<'a> {
    /// Outside the `.internal` zone.
    Foreign,
    /// In the zone. `selector` is what precedes the zone, and the Internal DNS alias's Cluster
    /// label when the name ends in `.ployz.internal.`; `selector` then excludes both.
    Under {
        selector: &'a str,
        alias: Option<&'a str>,
    },
}

impl<'a> Fqdn<'a> {
    #[must_use]
    pub fn parse(fqdn: &'a str) -> Self {
        let selector = if fqdn == format!("{ZONE}.") {
            ""
        } else if let Some(selector) = fqdn.strip_suffix(&format!(".{ZONE}.")) {
            selector
        } else {
            return Self::Foreign;
        };
        match selector.strip_suffix(&format!(".{}", Namespace::ALIAS)) {
            None => Self::Under {
                selector,
                alias: None,
            },
            Some(aliased) => {
                let (selector, label) = aliased.rsplit_once('.').unwrap_or(("", aliased));
                Self::Under {
                    selector,
                    alias: Some(label),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(name: &str) -> ServiceName {
        ServiceName::parse(name).unwrap()
    }

    fn namespace(name: &str) -> Namespace {
        Namespace::parse(name).unwrap()
    }

    #[test]
    fn constructors_build_the_strings_containers_resolve() {
        assert_eq!(short(&service("shop")), "shop.internal");
        assert_eq!(search_domain(&namespace("shop")), "shop.internal");
        assert_eq!(alias_wildcard("acme"), "*.acme.ployz.internal");
    }

    #[test]
    fn only_a_one_label_host_gains_the_namespace() {
        let prod = namespace("prod");
        assert_eq!(qualify("api.internal", &prod), "api.prod.internal");
        assert_eq!(qualify("api.app.internal", &prod), "api.app.internal");
        assert_eq!(qualify("api.example.com", &prod), "api.example.com");
        assert_eq!(qualify("internal", &prod), "internal");
    }

    #[test]
    fn parse_splits_the_zone_and_the_alias() {
        let under = |selector, alias| Fqdn::Under { selector, alias };
        assert_eq!(Fqdn::parse("internal."), under("", None));
        assert_eq!(Fqdn::parse("web.internal."), under("web", None));
        assert_eq!(
            Fqdn::parse("web.shop-staging.nearest.internal."),
            under("web.shop-staging.nearest", None)
        );
        assert_eq!(
            Fqdn::parse("postgres.prod.acme.ployz.internal."),
            under("postgres.prod", Some("acme"))
        );
        assert_eq!(Fqdn::parse("acme.ployz.internal."), under("", Some("acme")));
        assert_eq!(Fqdn::parse(".ployz.internal."), under("", Some("")));
        assert_eq!(Fqdn::parse("ployz.internal."), under("ployz", None));
        assert_eq!(Fqdn::parse(".internal."), under("", None));
    }

    #[test]
    fn parse_leaves_names_outside_the_zone_foreign() {
        for name in [
            "example.com.",
            "api.internal.com.",
            "internal.example.",
            "api.internal",
            "internal",
            "xinternal.",
        ] {
            assert_eq!(Fqdn::parse(name), Fqdn::Foreign, "{name}");
        }
    }
}
