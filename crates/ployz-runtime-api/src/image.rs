use std::pin::Pin;

use async_trait::async_trait;
use ployz_model::{ImageDigest, ImagePlatform};
use thiserror::Error;
use tokio::io::AsyncRead;

pub type ImageArchiveReader = Pin<Box<dyn AsyncRead + Send + 'static>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeImage {
    pub reference: String,
    pub id: Option<String>,
    pub repo_digests: Vec<ImageDigest>,
    pub platform: Option<ImagePlatform>,
    pub size_bytes: Option<u64>,
}

impl RuntimeImage {
    /// Returns whether this runtime can verify the image by the requested
    /// digest identity. Docker archive import preserves the image id even when
    /// repository digests are absent, so local image pushes accept either
    /// repository digest or image id.
    #[must_use]
    pub fn has_digest(&self, digest: &ImageDigest) -> bool {
        self.repo_digests
            .iter()
            .any(|candidate| candidate == digest)
            || self.id.as_deref() == Some(digest.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeImageImportResult {
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageReceivePreflightRequest {
    pub expected_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageDiskPreflight {
    Unknown,
    Sufficient {
        available_bytes: u64,
        required_bytes: Option<u64>,
    },
    Insufficient {
        available_bytes: u64,
        required_bytes: u64,
    },
}

#[derive(Debug, Error)]
pub enum RuntimeImageError {
    #[error("image runtime capability '{capability}' is unsupported by backend '{backend}'")]
    UnsupportedCapability {
        backend: &'static str,
        capability: &'static str,
    },
    #[error("image '{reference}' was not found")]
    NotFound { reference: String },
    #[error("image '{reference}' has no digest")]
    MissingDigest { reference: String },
    #[error("image '{reference}' digest mismatch: expected {expected}, found {found}")]
    DigestMismatch {
        reference: String,
        expected: ImageDigest,
        found: String,
    },
    #[error("{operation}: {message}")]
    Backend {
        operation: &'static str,
        message: String,
    },
}

impl RuntimeImageError {
    #[must_use]
    pub fn unsupported(backend: &'static str, capability: &'static str) -> Self {
        Self::UnsupportedCapability {
            backend,
            capability,
        }
    }

    #[must_use]
    pub fn backend(operation: &'static str, message: impl Into<String>) -> Self {
        Self::Backend {
            operation,
            message: message.into(),
        }
    }
}

#[async_trait]
pub trait RuntimeImageBackend: Send + Sync {
    async fn inspect_image(
        &self,
        reference: &str,
    ) -> Result<Option<RuntimeImage>, RuntimeImageError>;

    async fn export_image_archive(
        &self,
        reference: &str,
    ) -> Result<ImageArchiveReader, RuntimeImageError> {
        let _ = reference;
        Err(RuntimeImageError::unsupported(
            "unknown",
            "image archive export",
        ))
    }

    async fn import_image_archive(
        &self,
        archive: ImageArchiveReader,
    ) -> Result<RuntimeImageImportResult, RuntimeImageError> {
        let _ = archive;
        Err(RuntimeImageError::unsupported(
            "unknown",
            "image archive import",
        ))
    }

    async fn verify_image_digest(
        &self,
        reference: &str,
        expected: &ImageDigest,
    ) -> Result<RuntimeImage, RuntimeImageError> {
        let Some(image) = self.inspect_image(reference).await? else {
            return Err(RuntimeImageError::NotFound {
                reference: reference.into(),
            });
        };
        if image.has_digest(expected) {
            return Ok(image);
        }
        let mut found = image
            .repo_digests
            .iter()
            .map(ImageDigest::as_str)
            .collect::<Vec<_>>();
        if let Some(id) = image.id.as_deref() {
            found.push(id);
        }
        let found = found.join(", ");
        Err(RuntimeImageError::DigestMismatch {
            reference: reference.into(),
            expected: expected.clone(),
            found,
        })
    }

    async fn preflight_image_receive(
        &self,
        request: ImageReceivePreflightRequest,
    ) -> Result<ImageDiskPreflight, RuntimeImageError> {
        let _ = request;
        Ok(ImageDiskPreflight::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_image_digest_matches_repo_digest_or_image_id() {
        let repo_digest =
            ImageDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("repo digest");
        let image_id =
            ImageDigest::try_new(format!("sha256:{}", "b".repeat(64))).expect("image id");
        let other = ImageDigest::try_new(format!("sha256:{}", "c".repeat(64))).expect("other");
        let image = RuntimeImage {
            reference: "example/app:latest".into(),
            id: Some(image_id.as_str().into()),
            repo_digests: vec![repo_digest.clone()],
            platform: None,
            size_bytes: None,
        };

        assert!(image.has_digest(&repo_digest));
        assert!(image.has_digest(&image_id));
        assert!(!image.has_digest(&other));
    }
}
