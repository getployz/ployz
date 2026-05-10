use std::pin::Pin;

use async_trait::async_trait;
use ployz_types::model::{ImageDigest, ImagePlatform};
use thiserror::Error;
use tokio::io::AsyncRead;

pub type ImageArchiveReader<'a> = Pin<Box<dyn AsyncRead + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeImage {
    pub reference: String,
    pub id: Option<String>,
    pub repo_digests: Vec<ImageDigest>,
    pub platform: Option<ImagePlatform>,
    pub size_bytes: Option<u64>,
}

impl RuntimeImage {
    #[must_use]
    pub fn has_digest(&self, digest: &ImageDigest) -> bool {
        self.repo_digests
            .iter()
            .any(|candidate| candidate == digest)
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

    async fn export_image_archive<'a>(
        &'a self,
        reference: &'a str,
    ) -> Result<ImageArchiveReader<'a>, RuntimeImageError> {
        let _ = reference;
        Err(RuntimeImageError::unsupported(
            "unknown",
            "image archive export",
        ))
    }

    async fn import_image_archive(
        &self,
        archive: ImageArchiveReader<'static>,
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
        let found = image
            .repo_digests
            .iter()
            .map(ImageDigest::as_str)
            .collect::<Vec<_>>()
            .join(", ");
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
