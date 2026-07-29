//! Bounded-retry startup state for the machine's Machine credential.
//!
//! `machine.seed` does not exist at install time on the first machine: ployzd
//! control writes it after activate-first-machine mints the `Machine{machine_id}`
//! user. Machine, gateway, and DNS processes therefore treat a missing seed
//! file as the typed [`MachineCredentialState::AwaitingSeedFile`] state —
//! re-reading the path on each bounded backoff tick with visible health —
//! instead of crash-looping or falling back to controller authority.

use std::path::PathBuf;
use std::time::Duration;

use ployz_nats::connect::NatsConnectConfig;

use crate::config::{RoleNatsConnect, SeedFileReadError};

/// Where a role process stands with its NATS credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineCredentialState {
    Ready(NatsConnectConfig),
    AwaitingSeedFile {
        path: PathBuf,
        attempts: u32,
        last_error: SeedFileReadError,
    },
}

/// Bounded backoff for seed-file pickup: pickup is an in-process re-read,
/// no restart is required or issued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeedFileRetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl SeedFileRetryPolicy {
    /// Roughly five minutes of waiting before the process exits with a
    /// typed error and supervision takes over.
    #[must_use]
    pub const fn default_policy() -> Self {
        Self {
            max_attempts: 60,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(10),
        }
    }
}

/// One read attempt expressed as the typed credential state.
#[must_use]
pub fn observe_role_credentials(
    connect: &RoleNatsConnect,
    attempts: u32,
) -> MachineCredentialState {
    match connect.read_connect_config() {
        Ok(config) => MachineCredentialState::Ready(config),
        Err(error) => MachineCredentialState::AwaitingSeedFile {
            path: connect.seed_file.clone(),
            attempts,
            last_error: error,
        },
    }
}

/// Re-reads the seed file with bounded backoff until it becomes readable
/// or the retry budget is exhausted. Each awaiting tick is reported on
/// stderr (`awaiting-credentials`) so the process health is visible rather
/// than a silent hang or a crash loop.
pub async fn await_role_credentials(
    process_name: &str,
    connect: &RoleNatsConnect,
    policy: &SeedFileRetryPolicy,
) -> Result<NatsConnectConfig, AwaitSeedFileError> {
    let mut backoff = policy.initial_backoff;
    let mut last_error: Option<SeedFileReadError> = None;
    for attempt in 1..=policy.max_attempts {
        match observe_role_credentials(connect, attempt) {
            MachineCredentialState::Ready(config) => return Ok(config),
            MachineCredentialState::AwaitingSeedFile {
                path,
                attempts,
                last_error: error,
            } => {
                tracing::warn!(
                    process = process_name,
                    path = %path.display(),
                    attempt = attempts,
                    max_attempts = policy.max_attempts,
                    error = %error,
                    "awaiting credentials seed file"
                );
                last_error = Some(error);
            }
        }
        if attempt < policy.max_attempts {
            tokio::time::sleep(backoff).await;
            backoff = backoff.saturating_mul(2).min(policy.max_backoff);
        }
    }

    Err(AwaitSeedFileError::AttemptsExhausted {
        path: connect.seed_file.clone(),
        attempts: policy.max_attempts,
        last_error: last_error.unwrap_or(SeedFileReadError::Missing {
            path: connect.seed_file.clone(),
        }),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AwaitSeedFileError {
    #[error(
        "NKey seed file {} did not become readable after {attempts} attempts: {last_error}",
        path.display()
    )]
    AttemptsExhausted {
        path: PathBuf,
        attempts: u32,
        last_error: SeedFileReadError,
    },
}
