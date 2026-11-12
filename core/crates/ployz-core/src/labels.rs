//! Docker label names Ployz writes on the containers it creates.

/// Docker label recording the creation key a container was made under.
pub const CREATION_KEY: &str = "ployz.creation.key";

#[cfg(test)]
mod tests {
    #[test]
    fn every_core_label_key_keeps_its_live_spelling() {
        assert_eq!(super::CREATION_KEY, "ployz.creation.key");
        assert_eq!(crate::MANAGED_LABEL, "ployz.managed");
        assert_eq!(crate::NAMESPACE_LABEL, "ployz.namespace");
        assert_eq!(crate::DEPLOYMENT_ID_LABEL, "ployz.deployment.id");
        assert_eq!(crate::MANAGEMENT_LABEL_PREFIX, "ployz.");
        assert_eq!(crate::config::SERVICE_ID_LABEL, "cloud.ployz.service.id");
    }
}
