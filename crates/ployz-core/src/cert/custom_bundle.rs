//! Validated custom-certificate material and its artifact digest.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::install::{InstallContractError, InstallSha256Digest};

use super::ActiveCertState;

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
