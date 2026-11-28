//! Which Services a Deployment targets, as its `services` column stores them: an empty
//! list is every Service. The Rust and SQL readers of that sentinel are here.

use ployz_core::ServiceName;

use super::Stored;

/// The Services a Deployment targets.
pub(crate) enum Targeting<N> {
    /// Every Service, Volume and Config; with `prune`, it also tears down every
    /// other Service running in its Namespace.
    Full { prune: bool },
    /// These Services and what they mount.
    Named(N),
}

impl<N> Targeting<N> {
    pub(crate) const fn is_full(&self) -> bool {
        matches!(self, Self::Full { .. })
    }

    pub(crate) const fn prunes(&self) -> bool {
        matches!(self, Self::Full { prune: true })
    }

    pub(crate) fn map<M>(self, named: impl FnOnce(N) -> M) -> Targeting<M> {
        match self {
            Self::Full { prune } => Targeting::Full { prune },
            Self::Named(services) => Targeting::Named(named(services)),
        }
    }
}

impl Stored {
    pub(crate) fn targeting(&self) -> Targeting<&[ServiceName]> {
        match self.summary.services.as_slice() {
            [] => Targeting::Full {
                prune: self.run.prune,
            },
            named => Targeting::Named(named),
        }
    }
}

/// Predicates on the `services` column of `config_deployment`.
pub(super) mod scope_sql {
    /// It targets every Service.
    pub(crate) const FULL: &str = "services = '[]'";

    /// It names its Services.
    pub(crate) const NAMED: &str = "services != '[]'";
}

#[cfg(test)]
mod tests {
    use ployz_core::ServiceName;

    use super::scope_sql::{FULL, NAMED};
    use crate::storage::json_text;

    #[test]
    fn the_predicates_read_the_text_an_empty_and_a_named_list_store_as() {
        let named = [ServiceName::parse("web").unwrap()];
        assert_eq!(json_text(&named), r#"["web"]"#);
        let empty = json_text(&Vec::<ServiceName>::new());
        assert_eq!(FULL, format!("services = '{empty}'"));
        assert_eq!(NAMED, format!("services != '{empty}'"));
    }
}
