//! Concrete Caddy deployment wiring for the Ingress Proxy.

use oci_client::{
    Client, ParseError, Reference, errors::OciDistributionError, secrets::RegistryAuth,
};
use semver::Version;
use thiserror::Error;

/// Failure while discovering the current Caddy image for ingress deployment.
#[derive(Debug, Error)]
pub enum IngressImageError {
    /// The configured image reference could not be parsed.
    #[error("parse Caddy image reference: {0}")]
    Reference(#[from] ParseError),
    /// Docker Hub tags could not be listed.
    #[error("list Docker Hub Caddy tags: {0}")]
    ListTags(#[from] OciDistributionError),
}

/// Discover the latest stable Caddy 2 image used for ingress.
///
/// # Errors
///
/// Returns [`IngressImageError`] when the image reference is invalid or Docker
/// Hub cannot list its tags.
pub async fn latest_image() -> Result<String, IngressImageError> {
    let reference = "docker.io/library/caddy:latest".parse::<Reference>()?;
    let response = Client::default()
        .list_tags(&reference, &RegistryAuth::Anonymous, None, None)
        .await?;
    Ok(select_image(&response.tags))
}

#[must_use]
fn select_image(tags: &[String]) -> String {
    tags.iter()
        .filter_map(|tag| {
            Version::parse(tag).ok().filter(|version| {
                version.major == 2
                    && version.pre.is_empty()
                    && version.build.is_empty()
                    && version.to_string() == *tag
            })
        })
        .max()
        .map_or_else(
            || "caddy:latest".into(),
            |version| format!("caddy:{version}"),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_only_the_greatest_bare_two_x_y_tag() {
        assert_eq!(
            select_image(&[
                "2.9.1".into(),
                "2.10.0".into(),
                "2.11.0-rc.1".into(),
                "2.10".into(),
                "latest".into(),
                "3.0.0".into(),
            ]),
            "caddy:2.10.0"
        );
        assert_eq!(select_image(&["latest".into()]), "caddy:latest");
    }
}
