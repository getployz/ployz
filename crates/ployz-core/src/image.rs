use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct OciDigest(String);

impl OciDigest {
    pub fn try_new(value: impl Into<String>) -> Result<Self, OciDigestError> {
        let value = value.into();
        let Some(encoded) = value.strip_prefix("sha256:") else {
            return Err(OciDigestError::Invalid { value });
        };
        if encoded.len() != 64
            || !encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(OciDigestError::Invalid { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn sha256(bytes: &[u8]) -> Self {
        Self(format!("sha256:{:x}", Sha256::digest(bytes)))
    }
}
impl fmt::Display for OciDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<String> for OciDigest {
    type Error = OciDigestError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<OciDigest> for String {
    fn from(value: OciDigest) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OciDigestError {
    #[error("OCI digest {value:?} must be canonical sha256:<64 lowercase hex characters>")]
    Invalid { value: String },
}

/// One deploy-scoped registry credential. It may cross the operator and
/// machine RPC boundaries, but it is never part of deploy intent or evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RegistryCredential {
    Basic {
        username: RegistryCredentialUsername,
        password: RegistryCredentialSecret,
    },
    IdentityToken {
        token: RegistryCredentialSecret,
    },
}

impl RegistryCredential {
    pub fn try_basic(
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Result<Self, RegistryCredentialError> {
        Ok(Self::Basic {
            username: RegistryCredentialUsername::try_new(username)?,
            password: RegistryCredentialSecret::try_new(password)?,
        })
    }

    pub fn try_identity_token(token: impl Into<String>) -> Result<Self, RegistryCredentialError> {
        Ok(Self::IdentityToken {
            token: RegistryCredentialSecret::try_new(token)?,
        })
    }

    #[must_use]
    pub fn redact_secret_in(&self, message: impl Into<String>) -> String {
        let secret = match self {
            Self::Basic {
                username: _,
                password,
            } => password,
            Self::IdentityToken { token } => token,
        };
        message.into().replace(secret.secret(), "[redacted]")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct RegistryCredentialUsername(String);

impl RegistryCredentialUsername {
    pub fn try_new(value: impl Into<String>) -> Result<Self, RegistryCredentialError> {
        let value = value.into();
        if value.is_empty() {
            return Err(RegistryCredentialError::EmptyUsername);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RegistryCredentialUsername {
    type Error = RegistryCredentialError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<RegistryCredentialUsername> for String {
    fn from(value: RegistryCredentialUsername) -> Self {
        let RegistryCredentialUsername(value) = value;
        value
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct RegistryCredentialSecret(String);

impl RegistryCredentialSecret {
    pub fn try_new(value: impl Into<String>) -> Result<Self, RegistryCredentialError> {
        let value = value.into();
        if value.is_empty() {
            return Err(RegistryCredentialError::EmptySecret);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn secret(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RegistryCredentialSecret {
    type Error = RegistryCredentialError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<RegistryCredentialSecret> for String {
    fn from(value: RegistryCredentialSecret) -> Self {
        let RegistryCredentialSecret(value) = value;
        value
    }
}

impl fmt::Debug for RegistryCredentialSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(_secret) = self;
        formatter.write_str("RegistryCredentialSecret([redacted])")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryCredentialError {
    #[error("registry credential username is empty")]
    EmptyUsername,
    #[error("registry credential secret is empty")]
    EmptySecret,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(try_from = "OciPlatformWire", into = "OciPlatformWire")]
pub struct OciPlatform {
    os: String,
    architecture: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OciPlatformWire {
    os: String,
    architecture: String,
}

impl OciPlatform {
    pub fn try_new(
        os: impl Into<String>,
        architecture: impl Into<String>,
    ) -> Result<Self, OciPlatformError> {
        let os = normalize_platform_component("os", os.into())?;
        let architecture = normalize_platform_component("architecture", architecture.into())?;
        let architecture = match architecture.as_str() {
            "x86_64" | "x86-64" => "amd64".to_owned(),
            "aarch64" => "arm64".to_owned(),
            _ => architecture,
        };
        Ok(Self { os, architecture })
    }

    #[must_use]
    pub fn current() -> Self {
        Self::try_new(std::env::consts::OS, std::env::consts::ARCH)
            .expect("Rust target OS and architecture form a valid OCI platform")
    }

    #[must_use]
    pub fn os(&self) -> &str {
        &self.os
    }

    #[must_use]
    pub fn architecture(&self) -> &str {
        &self.architecture
    }
}

impl TryFrom<OciPlatformWire> for OciPlatform {
    type Error = OciPlatformError;

    fn try_from(value: OciPlatformWire) -> Result<Self, Self::Error> {
        Self::try_new(value.os, value.architecture)
    }
}

impl From<OciPlatform> for OciPlatformWire {
    fn from(value: OciPlatform) -> Self {
        let OciPlatform { os, architecture } = value;
        Self { os, architecture }
    }
}

fn normalize_platform_component(
    field: &'static str,
    value: String,
) -> Result<String, OciPlatformError> {
    let value = value.to_ascii_lowercase();
    if value.is_empty() {
        return Err(OciPlatformError::Empty { field });
    }
    if value.chars().any(|character| {
        !character.is_ascii_lowercase()
            && !character.is_ascii_digit()
            && !matches!(character, '-' | '_' | '.' | '+')
    }) {
        return Err(OciPlatformError::InvalidCharacter { field, value });
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OciPlatformError {
    #[error("OCI platform {field} must not be empty")]
    Empty { field: &'static str },
    #[error("OCI platform {field} contains an invalid character: {value:?}")]
    InvalidCharacter { field: &'static str, value: String },
}

#[cfg(test)]
mod oci_platform_tests {
    use super::*;

    #[test]
    fn platform_normalizes_case_and_architecture_aliases() {
        assert_eq!(
            OciPlatform::try_new("LINUX", "x86_64").expect("platform"),
            OciPlatform::try_new("linux", "amd64").expect("platform")
        );
        assert_eq!(
            OciPlatform::try_new("Linux", "AARCH64").expect("platform"),
            OciPlatform::try_new("linux", "arm64").expect("platform")
        );
    }

    #[test]
    fn platform_wire_cannot_bypass_validation() {
        assert!(
            serde_json::from_str::<OciPlatform>(r#"{"os":"","architecture":"amd64"}"#).is_err()
        );
        assert!(
            serde_json::from_str::<OciPlatform>(r#"{"os":"linux","architecture":"amd 64"}"#)
                .is_err()
        );
        assert_eq!(
            serde_json::from_str::<OciPlatform>(r#"{"os":"LINUX","architecture":"x86_64"}"#)
                .expect("normalized platform"),
            OciPlatform::try_new("linux", "amd64").expect("platform")
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageRemoveOutcome {
    Removed,
    AlreadyAbsent,
    RetainedInUse,
}

#[cfg(test)]
mod tests {
    use super::OciDigest;

    #[test]
    fn oci_digest_accepts_a_canonical_sha256_digest() {
        let value = format!("sha256:{}", "a".repeat(64));

        let digest = OciDigest::try_new(value.clone()).expect("valid digest");

        assert_eq!(digest.as_str(), value);
    }

    #[test]
    fn oci_digest_rejects_non_canonical_values() {
        let values = [
            String::new(),
            "a".repeat(64),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}", "A".repeat(64)),
            format!("sha512:{}", "a".repeat(64)),
        ];

        for value in values {
            assert!(OciDigest::try_new(value).is_err());
        }
    }
}
