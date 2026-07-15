//! Gateway-local custom certificate artifacts.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::certificate::material::{
    CertificateMaterialError, certificate_material_path_for_digest,
    custom_certificate_material_path, load_custom_certificate, validate_custom_certificate,
    validate_custom_certificate_for_activation, write_custom_certificate,
};
use ployz_core::certificate::{
    ActiveCertState, CertificateArtifactPushRequest, CertificateArtifactRemoveRequest,
    CustomCertBundle,
};
use ployz_core::ids::CertId;
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

    pub fn load_at(
        &self,
        active: &ActiveCertState,
        now_seconds: u64,
    ) -> Result<CustomCertBundle, GatewayCertificateStoreError> {
        validate_custom_certificate_for_activation(active, now_seconds).map_err(store_error)?;
        self.load(active)
    }

    pub fn missing_at(
        &self,
        desired: &[ActiveCertState],
        now_seconds: u64,
    ) -> Result<Vec<CertId>, GatewayCertificateStoreError> {
        let mut cert_ids = BTreeSet::new();
        for active in desired {
            if !cert_ids.insert(active.cert_id.clone()) {
                return Err(GatewayCertificateStoreError::DuplicateDesiredCertificate {
                    cert_id: active.cert_id.clone(),
                });
            }
            validate_custom_certificate_for_activation(active, now_seconds).map_err(store_error)?;
        }

        let mut missing = Vec::new();
        for active in desired {
            let path =
                custom_certificate_material_path(&self.state_dir, active).map_err(store_error)?;
            match path.try_exists() {
                Ok(false) => {
                    missing.push(active.cert_id.clone());
                    continue;
                }
                Ok(true) => {}
                Err(error) => {
                    return Err(GatewayCertificateStoreError::ArtifactFile {
                        path,
                        message: error.to_string(),
                    });
                }
            }
            match self.load(active) {
                Ok(_) => {}
                Err(GatewayCertificateStoreError::InvalidMaterial { .. }) => {
                    missing.push(active.cert_id.clone());
                }
                Err(error) => return Err(error),
            }
        }
        Ok(missing)
    }

    /// Removes exactly the artifact named by certificate id and digest.
    /// Missing artifacts are already converged and succeed.
    pub fn remove(
        &self,
        request: &CertificateArtifactRemoveRequest,
    ) -> Result<(), GatewayCertificateStoreError> {
        let path = certificate_material_path_for_digest(
            &self.state_dir,
            &request.cert_id,
            &request.digest,
        );
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(GatewayCertificateStoreError::ArtifactFile {
                path,
                message: error.to_string(),
            }),
        }
    }

    #[cfg(test)]
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
    #[error("desired certificate set contains duplicate id {}", cert_id.as_str())]
    DuplicateDesiredCertificate { cert_id: CertId },
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

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ids::{CertId, OperationId};

    #[test]
    fn remove_is_exact_and_idempotent() {
        let state = tempfile::tempdir().expect("gateway state");
        let store = GatewayCertificateStore::new(state.path().to_path_buf());
        let request = CertificateArtifactRemoveRequest {
            operation_id: OperationId::try_new("op_remove_cert").expect("operation id"),
            cert_id: CertId::try_new("cert_example").expect("cert id"),
            digest: InstallSha256Digest::try_new("a".repeat(64)).expect("digest"),
        };
        let retained = certificate_material_path_for_digest(
            state.path(),
            &request.cert_id,
            &InstallSha256Digest::try_new("b".repeat(64)).expect("retained digest"),
        );
        let removed =
            certificate_material_path_for_digest(state.path(), &request.cert_id, &request.digest);
        std::fs::create_dir_all(removed.parent().expect("bundle directory"))
            .expect("create bundle directory");
        std::fs::write(&removed, b"old").expect("write removed artifact");
        std::fs::write(&retained, b"new").expect("write retained artifact");

        store.remove(&request).expect("remove artifact");
        store.remove(&request).expect("repeat removal");

        assert!(!removed.exists());
        assert!(retained.exists());
    }
}
