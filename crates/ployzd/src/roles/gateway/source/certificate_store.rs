//! Gateway-local custom certificate artifacts.

use std::path::PathBuf;

use crate::certificate::material::{
    CertificateMaterialError, custom_certificate_material_path, load_custom_certificate,
    validate_custom_certificate, validate_custom_certificate_for_activation,
    write_custom_certificate,
};
use ployz_core::cert::{ActiveCertState, CertificateArtifactPushRequest, CustomCertBundle};
use ployz_core::install::InstallSha256Digest;

#[derive(Debug, Clone)]
pub struct GatewayCertificateStore {
    state_dir: PathBuf,
}

impl GatewayCertificateStore {
    #[must_use]
    pub fn new(state_dir: PathBuf) -> Self {
        Self { state_dir }
    }

    pub fn push_at(
        &self,
        request: &CertificateArtifactPushRequest,
        now_seconds: u64,
    ) -> Result<(), GatewayCertificateStoreError> {
        let bundle = &request.bundle;
        let material = bundle.material_bytes();
        let actual_size = u64::try_from(material.len()).map_err(|error| {
            GatewayCertificateStoreError::InvalidMaterial {
                message: error.to_string(),
            }
        })?;
        if request.expected_size != actual_size {
            return Err(GatewayCertificateStoreError::SizeMismatch {
                expected: request.expected_size,
                actual: actual_size,
            });
        }
        let (referenced_digest, _) =
            bundle
                .active_cert()
                .bundle_ref
                .artifact_parts()
                .map_err(|error| GatewayCertificateStoreError::InvalidMaterial {
                    message: error.to_string(),
                })?;
        if request.expected_digest != referenced_digest {
            return Err(GatewayCertificateStoreError::DigestMismatch {
                expected: request.expected_digest.clone(),
                referenced: referenced_digest,
            });
        }
        validate_custom_certificate(bundle).map_err(store_error)?;
        validate_custom_certificate_for_activation(bundle.active_cert(), now_seconds)
            .map_err(store_error)?;
        write_custom_certificate(&self.state_dir, bundle).map_err(store_error)
    }

    pub fn load(
        &self,
        active: &ActiveCertState,
    ) -> Result<CustomCertBundle, GatewayCertificateStoreError> {
        load_custom_certificate(&self.state_dir, active).map_err(store_error)
    }

    pub fn artifact_path(
        &self,
        active: &ActiveCertState,
    ) -> Result<PathBuf, GatewayCertificateStoreError> {
        custom_certificate_material_path(&self.state_dir, active).map_err(store_error)
    }
}

fn store_error(error: CertificateMaterialError) -> GatewayCertificateStoreError {
    match error {
        CertificateMaterialError::ArtifactFile { path, message } => {
            GatewayCertificateStoreError::ArtifactFile { path, message }
        }
        CertificateMaterialError::NonUtf8Path { path } => {
            GatewayCertificateStoreError::ArtifactFile {
                path,
                message: "certificate material path is not UTF-8".to_owned(),
            }
        }
        CertificateMaterialError::NotActivationEligible {
            now_seconds,
            not_before,
            not_after,
        } => GatewayCertificateStoreError::NotUsable {
            now_seconds,
            not_before,
            not_after,
        },
        error @ (CertificateMaterialError::InvalidMaterial { .. }
        | CertificateMaterialError::EmptyChain
        | CertificateMaterialError::InvalidChain { .. }
        | CertificateMaterialError::InvalidPrivateKey { .. }
        | CertificateMaterialError::KeyMismatch
        | CertificateMaterialError::HostnameMismatch { .. }
        | CertificateMaterialError::InvalidValidity { .. }
        | CertificateMaterialError::ValidityMismatch { .. }) => {
            GatewayCertificateStoreError::InvalidMaterial {
                message: error.to_string(),
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GatewayCertificateStoreError {
    #[error("certificate artifact size differs: expected {expected}, actual {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error(
        "certificate artifact digest differs: expected {}, referenced {}",
        expected.as_str(),
        referenced.as_str()
    )]
    DigestMismatch {
        expected: InstallSha256Digest,
        referenced: InstallSha256Digest,
    },
    #[error("invalid certificate material: {message}")]
    InvalidMaterial { message: String },
    #[error(
        "certificate is not usable at {now_seconds}; validity is {not_before} through {not_after}"
    )]
    NotUsable {
        now_seconds: u64,
        not_before: u64,
        not_after: u64,
    },
    #[error("certificate artifact file {}: {message}", path.display())]
    ArtifactFile { path: PathBuf, message: String },
}
