//! Validated custom-certificate material and its artifact digest.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ids::CertId;
use crate::install::{InstallContractError, InstallSha256Digest};
use crate::ops::RouteHostname;

use super::{CertBundleRef, CertValidityWindow, refresh_due};

/// Active certificate intent/evidence value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ActiveCertState {
    pub cert_id: CertId,
    pub hostname: RouteHostname,
    pub bundle_ref: CertBundleRef,
    pub validity: CertValidityWindow,
}

impl ActiveCertState {
    /// A certificate is eligible for activation or artifact push only inside
    /// its declared validity window. A gateway may keep serving expired
    /// last-known-good material until a replacement is available.
    #[must_use]
    pub fn is_usable_at(&self, now_seconds: u64) -> bool {
        self.validity.not_before.unix_seconds() <= now_seconds
            && now_seconds < self.validity.not_after.unix_seconds()
    }

    /// Custom certificates renew once two thirds of their validity has elapsed.
    #[must_use]
    pub fn needs_renewal(&self, now_seconds: u64) -> bool {
        refresh_due(
            self.validity.not_before.unix_seconds(),
            self.validity.not_after.unix_seconds(),
            now_seconds,
        )
    }
}

/// Custom certificate material stored behind an active certificate reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "CustomCertBundleWire", into = "CustomCertBundleWire")]
pub struct CustomCertBundle {
    active_cert: ActiveCertState,
    certificate_chain_pem: String,
    private_key_pem: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomCertBundleWire {
    active_cert: ActiveCertState,
    certificate_chain_pem: String,
    private_key_pem: String,
}

impl CustomCertBundle {
    pub fn try_new(
        active_cert: ActiveCertState,
        certificate_chain_pem: String,
        private_key_pem: String,
    ) -> Result<Self, CustomCertBundleError> {
        if certificate_chain_pem.as_bytes().contains(&0) || private_key_pem.as_bytes().contains(&0)
        {
            return Err(CustomCertBundleError::InvalidMaterialFraming);
        }
        let expected = custom_bundle_digest(&certificate_chain_pem, &private_key_pem)?;
        let (referenced, _) = active_cert.bundle_ref.artifact_parts()?;
        if referenced != expected {
            return Err(CustomCertBundleError::DigestMismatch);
        }

        Ok(Self {
            active_cert,
            certificate_chain_pem,
            private_key_pem,
        })
    }

    pub fn try_from_material_bytes(
        active_cert: ActiveCertState,
        material: &[u8],
    ) -> Result<Self, CustomCertBundleError> {
        let Some(separator) = material.iter().position(|byte| *byte == 0) else {
            return Err(CustomCertBundleError::InvalidMaterialFraming);
        };
        if material[separator + 1..].contains(&0) {
            return Err(CustomCertBundleError::InvalidMaterialFraming);
        }
        let certificate_chain_pem = std::str::from_utf8(&material[..separator])
            .map_err(|error| CustomCertBundleError::InvalidMaterialUtf8 {
                message: error.to_string(),
            })?
            .to_owned();
        let private_key_pem = std::str::from_utf8(&material[separator + 1..])
            .map_err(|error| CustomCertBundleError::InvalidMaterialUtf8 {
                message: error.to_string(),
            })?
            .to_owned();
        Self::try_new(active_cert, certificate_chain_pem, private_key_pem)
    }

    #[must_use]
    pub const fn active_cert(&self) -> &ActiveCertState {
        &self.active_cert
    }

    #[must_use]
    pub fn certificate_chain_pem(&self) -> &str {
        &self.certificate_chain_pem
    }

    #[must_use]
    pub fn private_key_pem(&self) -> &str {
        &self.private_key_pem
    }

    /// Bytes stored by the artifact named by the active certificate reference.
    #[must_use]
    pub fn material_bytes(&self) -> Vec<u8> {
        custom_bundle_material_bytes(&self.certificate_chain_pem, &self.private_key_pem)
    }

    #[must_use]
    pub fn into_parts(self) -> (ActiveCertState, String, String) {
        (
            self.active_cert,
            self.certificate_chain_pem,
            self.private_key_pem,
        )
    }
}

impl TryFrom<CustomCertBundleWire> for CustomCertBundle {
    type Error = CustomCertBundleError;

    fn try_from(value: CustomCertBundleWire) -> Result<Self, Self::Error> {
        let CustomCertBundleWire {
            active_cert,
            certificate_chain_pem,
            private_key_pem,
        } = value;
        Self::try_new(active_cert, certificate_chain_pem, private_key_pem)
    }
}

impl From<CustomCertBundle> for CustomCertBundleWire {
    fn from(value: CustomCertBundle) -> Self {
        let (active_cert, certificate_chain_pem, private_key_pem) = value.into_parts();
        Self {
            active_cert,
            certificate_chain_pem,
            private_key_pem,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CustomCertBundleError {
    #[error("custom cert bundle digest does not match its certificate and private key")]
    DigestMismatch,
    #[error("custom cert bundle digest is invalid: {0}")]
    Digest(#[from] InstallContractError),
    #[error("custom cert bundle reference is invalid: {0}")]
    BundleRef(#[from] super::CertTextError),
    #[error("custom cert bundle material must contain exactly one null separator")]
    InvalidMaterialFraming,
    #[error("custom cert bundle material is not UTF-8: {message}")]
    InvalidMaterialUtf8 { message: String },
}

/// Digest shared by the bundle reference and core-local artifact.
pub fn custom_bundle_digest(
    certificate_chain_pem: &str,
    private_key_pem: &str,
) -> Result<InstallSha256Digest, InstallContractError> {
    let digest = Sha256::digest(custom_bundle_material_bytes(
        certificate_chain_pem,
        private_key_pem,
    ));
    InstallSha256Digest::try_new(format!("{digest:x}"))
}

fn custom_bundle_material_bytes(certificate_chain_pem: &str, private_key_pem: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(certificate_chain_pem.len() + private_key_pem.len() + 1);
    bytes.extend_from_slice(certificate_chain_pem.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(private_key_pem.as_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cert::{CertBundleRef, CertValidAt, CertValidityWindow};
    use crate::ids::CertId;
    use crate::install::AbsoluteInstallPath;
    use crate::ops::RouteHostname;

    #[test]
    fn digest_valid_material_does_not_require_tls_parsing() {
        let bundle = bundle("not a certificate", "not a private key");

        assert_eq!(bundle.certificate_chain_pem(), "not a certificate");
        assert_eq!(bundle.private_key_pem(), "not a private key");
    }

    #[test]
    fn mismatched_artifact_digest_is_rejected() {
        let active_cert =
            active_cert(&InstallSha256Digest::try_new("a".repeat(64)).expect("digest"));

        assert_eq!(
            CustomCertBundle::try_new(
                active_cert,
                "certificate".to_owned(),
                "private-key".to_owned(),
            ),
            Err(CustomCertBundleError::DigestMismatch)
        );
    }

    #[test]
    fn material_bytes_roundtrip_through_the_validated_bundle() {
        let bundle = bundle("certificate", "private-key");

        assert_eq!(
            CustomCertBundle::try_from_material_bytes(
                bundle.active_cert().clone(),
                &bundle.material_bytes(),
            ),
            Ok(bundle)
        );
    }

    #[test]
    fn material_bytes_require_exactly_one_separator() {
        let digest = custom_bundle_digest("certificate", "private-key").expect("digest");

        assert_eq!(
            CustomCertBundle::try_from_material_bytes(
                active_cert(&digest),
                b"certificate\0private\0key",
            ),
            Err(CustomCertBundleError::InvalidMaterialFraming)
        );
    }

    #[test]
    fn serde_roundtrip_revalidates_private_material() {
        let bundle = bundle("certificate", "private-key");
        let encoded = serde_json::to_value(&bundle).expect("serialize bundle");

        assert_eq!(
            serde_json::from_value::<CustomCertBundle>(encoded).expect("deserialize bundle"),
            bundle
        );
    }

    fn bundle(certificate_chain_pem: &str, private_key_pem: &str) -> CustomCertBundle {
        let digest = custom_bundle_digest(certificate_chain_pem, private_key_pem).expect("digest");
        CustomCertBundle::try_new(
            active_cert(&digest),
            certificate_chain_pem.to_owned(),
            private_key_pem.to_owned(),
        )
        .expect("matching bundle")
    }

    fn active_cert(digest: &InstallSha256Digest) -> ActiveCertState {
        let path = AbsoluteInstallPath::try_new("/var/lib/ployz/certs/example.bundle")
            .expect("absolute path");
        ActiveCertState {
            cert_id: CertId::try_new("cert_example").expect("cert id"),
            hostname: RouteHostname::try_new("example.com").expect("hostname"),
            bundle_ref: CertBundleRef::for_bundle(digest, &path).expect("bundle ref"),
            validity: CertValidityWindow::try_new(
                CertValidAt::try_new(1_000).expect("not before"),
                CertValidAt::try_new(2_000).expect("not after"),
            )
            .expect("validity"),
        }
    }
}
