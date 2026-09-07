//! Corrosion process, API, and replicated-store integration.

mod admin;
mod api;
mod certificate;
mod publisher;
mod service;
mod store;

#[cfg(test)]
pub(crate) mod fake_cluster;
#[cfg(test)]
mod integration_tests;
#[cfg(test)]
mod store_tests;

pub(crate) use admin::membership_states_by_address;
pub use admin::{AdminClient, MembershipState};
use api::Statement;
pub(crate) use api::{ApiClient, Subscription};
pub use certificate::{
    CertificateChallenge, CertificateChallengeError, CertificateMaterial, CertificateMaterialError,
    CertificateRow,
};
pub use publisher::{run_machine_publisher, wait_for_catch_up};
pub use service::{CorrosionConfig, DEFAULT_CONTAINER_NAME, RunningCorrosion};
pub(crate) use store::{LocalContainerSnapshot, LocalVolumeSnapshot};
pub use store::{ReplicatedObservations, ReplicatedStore};

use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Corrosion I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Corrosion HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Corrosion JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid stored hosted DNS reservation: {0}")]
    InvalidDomainReservation(serde_json::Error),
    #[error(transparent)]
    Value(#[from] ployz_core::ValueError),
    #[error("Corrosion TOML failed: {0}")]
    Toml(#[from] toml::ser::Error),
    #[error("Docker operation for Corrosion failed: {0}")]
    Docker(#[from] bollard::errors::Error),
    #[error("{primary}; cleanup: {cleanup}")]
    CleanupAfter {
        primary: Box<Error>,
        cleanup: Box<Error>,
    },
    #[error("Corrosion API error: {0}")]
    Api(String),
    #[error("invalid Corrosion protocol response: {0}")]
    Protocol(String),
}

pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) async fn within_io_timeout<T>(
    work: impl std::future::Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    tokio::time::timeout(IO_TIMEOUT, work)
        .await
        .unwrap_or_else(|_| {
            Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Corrosion I/O timed out",
            )))
        })
}
