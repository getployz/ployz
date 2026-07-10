//! Gateway-local custom certificate artifacts.

use std::path::{Path, PathBuf};

use crate::adapters::atomic_file::write_file_atomically;
use crate::certificate::material::validate_and_read_validity;
use ployz_core::cert::{
    ActiveCertState, CertificateArtifactKind, CertificateArtifactPushOutcome,
    CertificateArtifactPushRequest, CustomCertBundle, custom_bundle_digest,
};
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
    ) -> Result<CertificateArtifactPushOutcome, GatewayCertificateStoreError> {
        let CertificateArtifactKind::CustomTlsBundle = request.artifact_kind;
        let bundle = &request.bundle;
        let active = bundle.active_cert();
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
        let actual_digest =
            custom_bundle_digest(bundle.certificate_chain_pem(), bundle.private_key_pem())
                .map_err(|error| GatewayCertificateStoreError::InvalidMaterial {
                    message: error.to_string(),
                })?;
        let (referenced_digest, _) = active.bundle_ref.artifact_parts().map_err(|error| {
            GatewayCertificateStoreError::InvalidMaterial {
                message: error.to_string(),
            }
        })?;
        if request.expected_digest != referenced_digest || actual_digest != referenced_digest {
            return Err(GatewayCertificateStoreError::DigestMismatch {
                expected: request.expected_digest.clone(),
                referenced: referenced_digest,
                actual: actual_digest,
            });
        }
        validate_bundle(bundle, now_seconds)?;

        let path = self.artifact_path(active)?;
        if std::fs::read(&path).is_ok_and(|existing| existing == material)
            && self.load_at(active, now_seconds).is_ok()
        {
            return Ok(CertificateArtifactPushOutcome::AlreadyPresent);
        }
        write_secret_bundle(&path, &material)?;
        Ok(CertificateArtifactPushOutcome::Stored)
    }

    pub fn load_at(
        &self,
        active: &ActiveCertState,
        now_seconds: u64,
    ) -> Result<CustomCertBundle, GatewayCertificateStoreError> {
        let path = self.artifact_path(active)?;
        let material =
            std::fs::read(&path).map_err(|error| GatewayCertificateStoreError::ArtifactFile {
                path: path.clone(),
                message: error.to_string(),
            })?;
        let Some(parent) = path.parent() else {
            return Err(GatewayCertificateStoreError::ArtifactFile {
                path,
                message: "artifact path has no parent directory".to_owned(),
            });
        };
        restrict_permissions(parent, &path)?;
        let Some(separator) = material.iter().position(|byte| *byte == 0) else {
            return Err(GatewayCertificateStoreError::InvalidMaterial {
                message: "certificate artifact has no material separator".to_owned(),
            });
        };
        let certificate_chain_pem =
            String::from_utf8(material[..separator].to_vec()).map_err(|error| {
                GatewayCertificateStoreError::InvalidMaterial {
                    message: error.to_string(),
                }
            })?;
        let private_key_pem =
            String::from_utf8(material[separator + 1..].to_vec()).map_err(|error| {
                GatewayCertificateStoreError::InvalidMaterial {
                    message: error.to_string(),
                }
            })?;
        let bundle =
            CustomCertBundle::try_new(active.clone(), certificate_chain_pem, private_key_pem)
                .map_err(|error| GatewayCertificateStoreError::InvalidMaterial {
                    message: error.to_string(),
                })?;
        validate_bundle(&bundle, now_seconds)?;
        Ok(bundle)
    }

    pub fn artifact_path(
        &self,
        active: &ActiveCertState,
    ) -> Result<PathBuf, GatewayCertificateStoreError> {
        let (digest, _) = active.bundle_ref.artifact_parts().map_err(|error| {
            GatewayCertificateStoreError::InvalidMaterial {
                message: error.to_string(),
            }
        })?;
        Ok(self.state_dir.join("bundles").join(format!(
            "{}-{}.bundle",
            active.cert_id.as_str(),
            digest.as_str()
        )))
    }
}

fn validate_bundle(
    bundle: &CustomCertBundle,
    now_seconds: u64,
) -> Result<(), GatewayCertificateStoreError> {
    let active = bundle.active_cert();
    let actual_validity = validate_and_read_validity(
        bundle.certificate_chain_pem(),
        bundle.private_key_pem(),
        &active.hostname,
    )
    .map_err(|error| GatewayCertificateStoreError::InvalidMaterial {
        message: error.to_string(),
    })?;
    if actual_validity != active.validity {
        return Err(GatewayCertificateStoreError::InvalidMaterial {
            message: format!(
                "certificate validity differs from active metadata: expected {:?}, actual {:?}",
                active.validity, actual_validity
            ),
        });
    }
    if !active.is_usable_at(now_seconds) {
        return Err(GatewayCertificateStoreError::NotUsable {
            now_seconds,
            not_before: active.validity.not_before.unix_seconds(),
            not_after: active.validity.not_after.unix_seconds(),
        });
    }
    Ok(())
}

fn write_secret_bundle(path: &Path, contents: &[u8]) -> Result<(), GatewayCertificateStoreError> {
    let Some(parent) = path.parent() else {
        return Err(GatewayCertificateStoreError::ArtifactFile {
            path: path.to_path_buf(),
            message: "artifact path has no parent directory".to_owned(),
        });
    };
    std::fs::create_dir_all(parent).map_err(|error| {
        GatewayCertificateStoreError::ArtifactFile {
            path: parent.to_path_buf(),
            message: error.to_string(),
        }
    })?;
    restrict_permissions(parent, path)?;
    write_file_atomically(path, contents).map_err(|error| {
        GatewayCertificateStoreError::ArtifactFile {
            path: path.to_path_buf(),
            message: error.to_string(),
        }
    })?;
    restrict_permissions(parent, path)
}

#[cfg(unix)]
fn restrict_permissions(directory: &Path, file: &Path) -> Result<(), GatewayCertificateStoreError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).map_err(
        |error| GatewayCertificateStoreError::ArtifactFile {
            path: directory.to_path_buf(),
            message: error.to_string(),
        },
    )?;
    if file.exists() {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).map_err(
            |error| GatewayCertificateStoreError::ArtifactFile {
                path: file.to_path_buf(),
                message: error.to_string(),
            },
        )?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(
    _directory: &Path,
    _file: &Path,
) -> Result<(), GatewayCertificateStoreError> {
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GatewayCertificateStoreError {
    #[error("certificate artifact size differs: expected {expected}, actual {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error(
        "certificate artifact digest differs: expected {}, referenced {}, actual {}",
        expected.as_str(),
        referenced.as_str(),
        actual.as_str()
    )]
    DigestMismatch {
        expected: InstallSha256Digest,
        referenced: InstallSha256Digest,
        actual: InstallSha256Digest,
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
