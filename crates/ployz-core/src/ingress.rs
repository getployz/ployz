//! Public ingress intent, route ownership, and endpoint projection models.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};

use crate::cert::ActiveCertState;
use crate::ids::RouteBindingId;
use crate::ops::{RouteHostname, RouteHostnameError};
use crate::reachability::is_public;
use crate::state::ControlPlaneEpoch;

/// Durable operator choice for automatic hostname creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum AutomaticHostnameConfiguration {
    Disabled,
    Ployz,
    Custom { suffix: AutomaticHostnameSuffix },
}

impl AutomaticHostnameConfiguration {
    pub fn custom(suffix: impl Into<String>) -> Result<Self, AutomaticHostnameConfigurationError> {
        Ok(Self::Custom {
            suffix: AutomaticHostnameSuffix::try_new(suffix)?,
        })
    }
}

/// Validated multi-label DNS suffix used for custom automatic hostnames.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"AutomaticHostnameSuffix\">")
)]
#[serde(try_from = "String", into = "String")]
pub struct AutomaticHostnameSuffix(RouteHostname);

impl AutomaticHostnameSuffix {
    pub fn try_new(suffix: impl Into<String>) -> Result<Self, AutomaticHostnameConfigurationError> {
        let suffix = suffix.into();
        let suffix = suffix
            .strip_suffix('.')
            .unwrap_or(&suffix)
            .to_ascii_lowercase();
        if suffix.split('.').count() < 2 {
            return Err(AutomaticHostnameConfigurationError::SingleLabel { suffix });
        }
        Ok(Self(RouteHostname::try_new(suffix)?))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    #[must_use]
    pub const fn as_hostname(&self) -> &RouteHostname {
        &self.0
    }
}

impl TryFrom<String> for AutomaticHostnameSuffix {
    type Error = AutomaticHostnameConfigurationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<AutomaticHostnameSuffix> for String {
    fn from(value: AutomaticHostnameSuffix) -> Self {
        value.0.into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AutomaticHostnameConfigurationError {
    #[error("custom automatic hostname suffix must contain at least two labels: {suffix}")]
    SingleLabel { suffix: String },
    #[error("custom automatic hostname suffix is invalid: {0}")]
    InvalidSuffix(#[from] RouteHostnameError),
}

/// Caller-selected label immediately beneath an automatic hostname suffix.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"AutomaticHostnameLabel\">")
)]
#[serde(try_from = "String", into = "String")]
pub struct AutomaticHostnameLabel(String);

impl AutomaticHostnameLabel {
    pub fn try_new(value: impl Into<String>) -> Result<Self, AutomaticHostnameLabelError> {
        let value = value.into().to_ascii_lowercase();
        if value.is_empty() {
            return Err(AutomaticHostnameLabelError::Empty);
        }
        if value.len() > 63 {
            return Err(AutomaticHostnameLabelError::TooLong { value });
        }
        if value.starts_with('-') || value.ends_with('-') {
            return Err(AutomaticHostnameLabelError::EdgeHyphen { value });
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(AutomaticHostnameLabelError::InvalidCharacter { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AutomaticHostnameLabel {
    type Error = AutomaticHostnameLabelError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<AutomaticHostnameLabel> for String {
    fn from(value: AutomaticHostnameLabel) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AutomaticHostnameLabelError {
    #[error("automatic hostname label is empty")]
    Empty,
    #[error("automatic hostname label exceeds 63 bytes: {value}")]
    TooLong { value: String },
    #[error("automatic hostname label has an edge hyphen: {value}")]
    EdgeHyphen { value: String },
    #[error("automatic hostname label contains an invalid character: {value}")]
    InvalidCharacter { value: String },
}

/// Durable operator choice for the Ployz-managed DNS target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum PloyzDnsTargetIntent {
    Enabled,
    Disabled,
}

/// Cluster-wide ingress configuration committed as one operator decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(
    try_from = "IngressConfigurationWire",
    into = "IngressConfigurationWire"
)]
pub struct IngressConfiguration {
    automatic_hostnames: AutomaticHostnameConfiguration,
    ployz_dns_target: PloyzDnsTargetIntent,
}

impl IngressConfiguration {
    pub fn try_new(
        automatic_hostnames: AutomaticHostnameConfiguration,
        ployz_dns_target: PloyzDnsTargetIntent,
    ) -> Result<Self, IngressConfigurationError> {
        if matches!(
            (&automatic_hostnames, ployz_dns_target),
            (
                AutomaticHostnameConfiguration::Ployz,
                PloyzDnsTargetIntent::Disabled
            )
        ) {
            return Err(IngressConfigurationError::PloyzHostnamesRequireDnsTarget);
        }
        Ok(Self {
            automatic_hostnames,
            ployz_dns_target,
        })
    }

    #[must_use]
    pub const fn automatic_hostnames(&self) -> &AutomaticHostnameConfiguration {
        &self.automatic_hostnames
    }

    #[must_use]
    pub const fn ployz_dns_target(&self) -> PloyzDnsTargetIntent {
        self.ployz_dns_target
    }

    #[must_use]
    pub fn into_parts(self) -> (AutomaticHostnameConfiguration, PloyzDnsTargetIntent) {
        (self.automatic_hostnames, self.ployz_dns_target)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
struct IngressConfigurationWire {
    automatic_hostnames: AutomaticHostnameConfiguration,
    ployz_dns_target: PloyzDnsTargetIntent,
}

impl TryFrom<IngressConfigurationWire> for IngressConfiguration {
    type Error = IngressConfigurationError;

    fn try_from(value: IngressConfigurationWire) -> Result<Self, Self::Error> {
        Self::try_new(value.automatic_hostnames, value.ployz_dns_target)
    }
}

impl From<IngressConfiguration> for IngressConfigurationWire {
    fn from(value: IngressConfiguration) -> Self {
        let (automatic_hostnames, ployz_dns_target) = value.into_parts();
        Self {
            automatic_hostnames,
            ployz_dns_target,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IngressConfigurationError {
    #[error("Ployz automatic hostnames require the Ployz DNS target")]
    PloyzHostnamesRequireDnsTarget,
}

/// Stable provenance of one attached Route Binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum RouteBindingOrigin {
    Declared,
    Automatic,
}

/// Owner whose lifecycle controls active certificate metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "owner", rename_all = "snake_case", deny_unknown_fields)]
pub enum CertificateOwner {
    PloyzAutomaticNamespace,
    RouteBinding { route_binding_id: RouteBindingId },
}

/// Public, non-secret metadata for the active certificate owned by one ingress resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ActiveCertificateMetadata {
    pub owner: CertificateOwner,
    pub active: ActiveCertState,
}

/// Non-empty normalized public endpoint set for cluster ingress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(try_from = "IngressEndpointSetWire", into = "IngressEndpointSetWire")]
pub struct IngressEndpointSet {
    ipv4: BTreeSet<Ipv4Addr>,
    ipv6: BTreeSet<Ipv6Addr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
struct IngressEndpointSetWire {
    ipv4: Vec<Ipv4Addr>,
    ipv6: Vec<Ipv6Addr>,
}

impl IngressEndpointSet {
    pub fn try_new(
        ipv4: impl IntoIterator<Item = Ipv4Addr>,
        ipv6: impl IntoIterator<Item = Ipv6Addr>,
    ) -> Result<Self, IngressEndpointSetError> {
        let ipv4 = ipv4.into_iter().collect::<BTreeSet<_>>();
        let ipv6 = ipv6.into_iter().collect::<BTreeSet<_>>();
        if ipv4.is_empty() && ipv6.is_empty() {
            return Err(IngressEndpointSetError::Empty);
        }
        if let Some(address) = ipv4
            .iter()
            .copied()
            .map(IpAddr::V4)
            .chain(ipv6.iter().copied().map(IpAddr::V6))
            .find(|address| !is_public(*address))
        {
            return Err(IngressEndpointSetError::NotPublic { address });
        }
        Ok(Self { ipv4, ipv6 })
    }

    #[must_use]
    pub const fn ipv4(&self) -> &BTreeSet<Ipv4Addr> {
        &self.ipv4
    }

    #[must_use]
    pub const fn ipv6(&self) -> &BTreeSet<Ipv6Addr> {
        &self.ipv6
    }
}

impl TryFrom<IngressEndpointSetWire> for IngressEndpointSet {
    type Error = IngressEndpointSetError;

    fn try_from(value: IngressEndpointSetWire) -> Result<Self, Self::Error> {
        Self::try_new(value.ipv4, value.ipv6)
    }
}

impl From<IngressEndpointSet> for IngressEndpointSetWire {
    fn from(value: IngressEndpointSet) -> Self {
        Self {
            ipv4: value.ipv4.into_iter().collect(),
            ipv6: value.ipv6.into_iter().collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IngressEndpointSetError {
    #[error("ingress endpoint set is empty")]
    Empty,
    #[error("ingress endpoint is not public: {address}")]
    NotPublic { address: IpAddr },
}

/// High-level reason that ingress endpoint publication must be withdrawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum IngressEndpointUnavailableReason {
    NoDeclaredGateways,
    NoPublishableEndpoints,
}

/// Availability of the complete ingress endpoint projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum IngressEndpointProjectionState {
    Pending,
    Current {
        endpoints: IngressEndpointSet,
    },
    Retained {
        endpoints: IngressEndpointSet,
    },
    Unavailable {
        reason: IngressEndpointUnavailableReason,
    },
}

/// Identity carried by ingress projection invalidations and checkpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct IngressEndpointProjectionIdentity {
    pub control_plane_epoch: ControlPlaneEpoch,
    pub revision: u64,
}

/// Core-owned complete public ingress endpoint projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct IngressEndpointProjection {
    pub control_plane_epoch: ControlPlaneEpoch,
    pub revision: u64,
    pub state: IngressEndpointProjectionState,
}

impl IngressEndpointProjection {
    #[must_use]
    pub const fn identity(&self) -> IngressEndpointProjectionIdentity {
        IngressEndpointProjectionIdentity {
            control_plane_epoch: self.control_plane_epoch,
            revision: self.revision,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingress_configuration_rejects_ployz_hostnames_without_dns_target() {
        assert_eq!(
            IngressConfiguration::try_new(
                AutomaticHostnameConfiguration::Ployz,
                PloyzDnsTargetIntent::Disabled,
            ),
            Err(IngressConfigurationError::PloyzHostnamesRequireDnsTarget)
        );
        assert!(
            serde_json::from_value::<IngressConfiguration>(serde_json::json!({
                "automatic_hostnames": { "mode": "ployz" },
                "ployz_dns_target": "disabled"
            }))
            .is_err()
        );
    }

    #[test]
    fn automatic_label_accepts_one_lowercase_dns_label() {
        assert_eq!(
            AutomaticHostnameLabel::try_new("api-1")
                .expect("valid label")
                .as_str(),
            "api-1"
        );
    }

    #[test]
    fn automatic_label_lowercases_before_validation() {
        assert_eq!(
            AutomaticHostnameLabel::try_new("Api-V1")
                .expect("normalized label")
                .as_str(),
            "api-v1"
        );
    }

    #[test]
    fn automatic_label_rejects_edge_hyphens() {
        for value in ["-api", "api-"] {
            assert!(AutomaticHostnameLabel::try_new(value).is_err(), "{value}");
        }
    }

    #[test]
    fn custom_suffix_normalizes_case_and_one_root_dot() {
        assert_eq!(
            AutomaticHostnameConfiguration::custom("Apps.Example.COM.").expect("custom suffix"),
            AutomaticHostnameConfiguration::Custom {
                suffix: AutomaticHostnameSuffix::try_new("apps.example.com").expect("suffix"),
            }
        );
    }

    #[test]
    fn custom_suffix_rejects_single_label_and_invalid_dns_lengths() {
        assert!(AutomaticHostnameConfiguration::custom("localhost").is_err());
        assert!(
            AutomaticHostnameConfiguration::custom(format!("{}.example.com", "a".repeat(64)))
                .is_err()
        );
        assert!(
            AutomaticHostnameConfiguration::custom(format!(
                "{}.{}.{}.{}",
                "a".repeat(63),
                "b".repeat(63),
                "c".repeat(63),
                "d".repeat(62)
            ))
            .is_err()
        );
    }

    #[test]
    fn endpoint_set_sorts_deduplicates_and_round_trips() {
        let endpoints = IngressEndpointSet::try_new(
            [
                "8.8.8.8".parse().expect("ipv4"),
                "1.1.1.1".parse().expect("ipv4"),
                "8.8.8.8".parse().expect("ipv4"),
            ],
            ["2a01:4ff:2f0:296a::1".parse().expect("ipv6")],
        )
        .expect("public endpoints");
        let json = serde_json::to_string(&endpoints).expect("serialize endpoints");

        assert_eq!(
            serde_json::from_str::<IngressEndpointSet>(&json).expect("deserialize endpoints"),
            endpoints
        );
    }

    #[test]
    fn endpoint_set_rejects_empty_and_non_public_addresses() {
        assert_eq!(
            IngressEndpointSet::try_new(Vec::new(), Vec::new()),
            Err(IngressEndpointSetError::Empty)
        );
        assert_eq!(
            IngressEndpointSet::try_new(["10.0.0.1".parse().expect("ipv4")], Vec::new()),
            Err(IngressEndpointSetError::NotPublic {
                address: "10.0.0.1".parse().expect("ip"),
            })
        );
    }

    #[test]
    fn current_projection_cannot_decode_an_empty_endpoint_set() {
        let json = r#"{"status":"current","endpoints":{"ipv4":[],"ipv6":[]}}"#;

        assert!(serde_json::from_str::<IngressEndpointProjectionState>(json).is_err());
    }
}
