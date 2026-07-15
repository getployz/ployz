//! Validation failures and shared validation policy for install contracts.

use url::{Host, Url};

use crate::nats_config::is_valid_host_syntax;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallContractError {
    #[error("cluster name is empty")]
    EmptyClusterName,
    #[error("cluster name {value:?} contains unsupported characters")]
    InvalidClusterName { value: String },
    #[error("machine bootstrap URL is empty")]
    EmptyBootstrapUrl,
    #[error("machine bootstrap URL {value:?} must be an HTTPS URL without whitespace")]
    InvalidBootstrapUrl { value: String },
    #[error("runtime NATS URL is empty")]
    EmptyRuntimeNatsUrl,
    #[error("runtime NATS URL {value:?} must be a nats:// or tls:// URL with host and port")]
    InvalidRuntimeNatsUrl { value: String },
    #[error("artifact version is empty")]
    EmptyArtifactVersion,
    #[error("artifact source is empty")]
    EmptyArtifactSource,
    #[error("artifact source path {value} must be absolute")]
    RelativeArtifactSource { value: String },
    #[error("sha256 digest is empty")]
    EmptySha256Digest,
    #[error("sha256 digest {value:?} must be 64 hex characters")]
    InvalidSha256Digest { value: String },
    #[error("install path is empty")]
    EmptyInstallPath,
    #[error("install path {value} must be absolute")]
    RelativeInstallPath { value: String },
    #[error("install path {value} must include a parent")]
    MissingInstallParent { value: String },
    #[error("install path {value} must include a file name")]
    MissingInstallFileName { value: String },
}

pub(super) fn nats_url_has_host_and_port(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    if !matches!(url.scheme(), "nats" | "tls")
        || !url.username().is_empty()
        || url.password().is_some()
        || !url.path().is_empty()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let Some(host) = url.host() else {
        return false;
    };
    let Some(port) = url.port() else {
        return false;
    };
    port > 0
        && match host {
            Host::Domain(host) => is_valid_host_syntax(host),
            Host::Ipv4(_) | Host::Ipv6(_) => true,
        }
}

pub(super) fn has_invisible_characters(value: &str) -> bool {
    value
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
}
