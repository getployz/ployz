//! Route naming and ownership values shared by the V2 API and gateway.

use serde::{Deserialize, Serialize};

/// Caller-selected label immediately beneath an automatic hostname suffix.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"AutomaticHostnameLabel\">"))]
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

/// Stable provenance of one attached route binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum RouteBindingOrigin {
    Declared,
    Automatic,
}

#[cfg(test)]
mod tests {
    use super::AutomaticHostnameLabel;

    #[test]
    fn automatic_hostname_labels_are_canonical_dns_labels() {
        assert_eq!(
            AutomaticHostnameLabel::try_new("Api-V1")
                .expect("label")
                .as_str(),
            "api-v1"
        );
        for invalid in ["", "-api", "api-", "api_v1"] {
            assert!(AutomaticHostnameLabel::try_new(invalid).is_err());
        }
    }
}
