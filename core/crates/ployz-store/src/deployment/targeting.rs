//! Which Services a Deployment targets, as its `services` column stores them: an empty
//! list is every Service. The Rust and SQL readers of that sentinel are here.

use ployz_core::ServiceName;

use super::Stored;

/// The Services a Deployment targets.
pub(crate) enum Targeting<'a> {
    /// Every Service, Volume and Config.
    Full,
    /// These Services and what they mount.
    Named(&'a [ServiceName]),
}

impl Targeting<'_> {
    pub(crate) fn is_full(&self) -> bool {
        matches!(self, Self::Full)
    }
}

impl Stored {
    pub(crate) fn targeting(&self) -> Targeting<'_> {
        match self.summary.services.as_slice() {
            [] => Targeting::Full,
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
        assert_eq!(json_text(&Vec::<ServiceName>::new()), "[]");
        assert_eq!(json_text(&named), r#"["web"]"#);
        assert_eq!(FULL, "services = '[]'");
        assert_eq!(NAMED, "services != '[]'");
    }
}
