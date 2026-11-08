//! Pin an image tag to the digest its registry serves it under, so a Deploy approves one
//! image and a Machine pulls exactly that. The CLI asks the registry itself, over TLS and
//! without credentials: Cloud never answers the lookup.

use std::time::Duration;

use oci_client::{
    Client, Reference,
    client::{ClientConfig, ClientProtocol},
    errors::{OciDistributionError, OciErrorCode},
    secrets::RegistryAuth,
};
use ployz_core::authority::deploy::content_addressed;
use thiserror::Error;

/// How long one registry lookup may take, token exchange included.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

/// An image reference that names its content: what the person wrote, then `@sha256:` and
/// the registry's index digest. Docker pulls the digest and ignores the tag before it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedImage(String);

impl PinnedImage {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

#[derive(Debug, Error)]
pub enum PinError {
    #[error("`{reference}` can't be pinned: it isn't a tag reference.")]
    NotATag { reference: String },
    /// The registry wants credentials to read the image, or doesn't say the image exists.
    #[error("{registry} wants credentials to read `{reference}`.")]
    NeedsCredentials { registry: String, reference: String },
    #[error("{registry} didn't answer within {} seconds.", LOOKUP_TIMEOUT.as_secs())]
    TimedOut { registry: String },
    #[error("{registry} didn't give a digest for `{reference}`.")]
    Unread {
        registry: String,
        reference: String,
        #[source]
        source: OciDistributionError,
    },
}

impl PinError {
    /// The registry the lookup went to.
    #[must_use]
    pub fn registry(&self) -> Option<&str> {
        match self {
            Self::NotATag { .. } => None,
            Self::NeedsCredentials { registry, .. }
            | Self::TimedOut { registry }
            | Self::Unread { registry, .. } => Some(registry),
        }
    }
}

/// Pin `reference`, a tag on a public registry, to the digest of its manifest index or
/// image manifest, whichever the tag names, so it matches what `docker pull` resolves.
///
/// # Errors
/// [`PinError::NeedsCredentials`] when the registry refuses an anonymous read,
/// [`PinError::TimedOut`] after ten seconds, [`PinError::Unread`] when it answers
/// without a usable digest or can't be reached, and [`PinError::NotATag`] for a
/// reference that is not parseable or already carries a digest.
pub async fn pin_public(reference: &str) -> Result<PinnedImage, PinError> {
    pin(reference, ClientProtocol::Https).await
}

async fn pin(reference: &str, protocol: ClientProtocol) -> Result<PinnedImage, PinError> {
    let not_a_tag = || PinError::NotATag {
        reference: reference.to_owned(),
    };
    if reference.contains('@') {
        return Err(not_a_tag());
    }
    let parsed: Reference = reference.parse().map_err(|_| not_a_tag())?;
    let registry = parsed.registry().to_owned();
    let client = Client::new(ClientConfig {
        protocol,
        connect_timeout: Some(LOOKUP_TIMEOUT),
        read_timeout: Some(LOOKUP_TIMEOUT),
        ..ClientConfig::default()
    });
    let digest = tokio::time::timeout(
        LOOKUP_TIMEOUT,
        client.fetch_manifest_digest(&parsed, &RegistryAuth::Anonymous),
    )
    .await
    .map_err(|_| PinError::TimedOut {
        registry: registry.clone(),
    })?
    .map_err(|source| refused(&registry, reference, source))?;
    let pinned = format!("{reference}@{digest}");
    if !content_addressed(&pinned) {
        return Err(PinError::Unread {
            registry,
            reference: reference.to_owned(),
            source: OciDistributionError::RegistryNoDigestError,
        });
    }
    Ok(PinnedImage(pinned))
}

fn refused(registry: &str, reference: &str, source: OciDistributionError) -> PinError {
    let wants_credentials = matches!(
        &source,
        OciDistributionError::AuthenticationFailure(_)
            | OciDistributionError::UnauthorizedError { .. }
            | OciDistributionError::ServerError {
                code: 401 | 403,
                ..
            }
    ) || matches!(
        &source,
        OciDistributionError::RegistryError { envelope, .. }
            if envelope.errors.iter().any(|error| {
                matches!(error.code, OciErrorCode::Unauthorized | OciErrorCode::Denied)
            })
    );
    if wants_credentials {
        PinError::NeedsCredentials {
            registry: registry.to_owned(),
            reference: reference.to_owned(),
        }
    } else {
        PinError::Unread {
            registry: registry.to_owned(),
            reference: reference.to_owned(),
            source,
        }
    }
}

#[cfg(test)]
#[path = "pin_tests.rs"]
mod tests;
