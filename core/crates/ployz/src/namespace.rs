//! Cluster-side Namespaces named on the command line.

use ployz_core::{Namespace, ValueError};
use thiserror::Error;

/// A Namespace that cannot be used.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum NamespaceError {
    #[error(transparent)]
    InvalidName(#[from] ValueError),
    #[error("Namespace '{name}' is reserved for Ployz infrastructure")]
    Reserved { name: Namespace },
}

/// Refuse a reserved Namespace on deployment and removal commands.
///
/// # Errors
///
/// Returns [`NamespaceError::Reserved`] when `name` is `ployz-system`.
pub fn refuse_reserved(name: &Namespace) -> Result<(), NamespaceError> {
    if name.is_reserved() {
        Err(NamespaceError::Reserved { name: name.clone() })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_names_parse_and_are_refused() {
        let name = Namespace::parse("ployz-system").unwrap();
        assert!(name.is_reserved());
        assert_eq!(
            refuse_reserved(&name).unwrap_err().to_string(),
            "Namespace 'ployz-system' is reserved for Ployz infrastructure"
        );
        refuse_reserved(&Namespace::parse("shop").unwrap()).unwrap();
    }
}
