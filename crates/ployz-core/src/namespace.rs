//! Namespace-wide policy shared by every namespace mutation surface.

use crate::ids::NamespaceId;

pub const RESERVED_SYSTEM_NAMESPACE_ID: &str = "ployz-system";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("namespace {} is reserved for Ployz system services", .namespace_id.as_str())]
pub struct ReservedSystemNamespace {
    pub namespace_id: NamespaceId,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "system deploy requires namespace {RESERVED_SYSTEM_NAMESPACE_ID}, not {}",
    .namespace_id.as_str()
)]
pub struct SystemNamespaceRequired {
    pub namespace_id: NamespaceId,
}

#[must_use]
pub fn reserved_system_namespace() -> NamespaceId {
    NamespaceId::try_new(RESERVED_SYSTEM_NAMESPACE_ID)
        .expect("the reserved system namespace is a valid namespace id")
}

pub fn ensure_ordinary_namespace(
    namespace_id: &NamespaceId,
) -> Result<(), ReservedSystemNamespace> {
    if namespace_id.as_str() == RESERVED_SYSTEM_NAMESPACE_ID {
        return Err(ReservedSystemNamespace {
            namespace_id: namespace_id.clone(),
        });
    }
    Ok(())
}

pub fn ensure_system_namespace(namespace_id: &NamespaceId) -> Result<(), SystemNamespaceRequired> {
    if namespace_id.as_str() != RESERVED_SYSTEM_NAMESPACE_ID {
        return Err(SystemNamespaceRequired {
            namespace_id: namespace_id.clone(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_namespace_policy_is_exact() {
        let reserved = reserved_system_namespace();
        assert_eq!(reserved.as_str(), "ployz-system");
        assert!(ensure_ordinary_namespace(&reserved).is_err());
        assert!(ensure_system_namespace(&reserved).is_ok());

        let prefix = NamespaceId::try_new("ployz-system-app").expect("namespace id");
        assert!(ensure_ordinary_namespace(&prefix).is_ok());
        assert!(ensure_system_namespace(&prefix).is_err());
    }
}
