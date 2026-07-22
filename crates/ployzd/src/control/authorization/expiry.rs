use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::control::intent::nats_authorizations::NatsAuthorizationStore;
use crate::process_support::BackoffSchedule;
use ployz_core::nats_config::{CredentialGrant, CredentialRole, NatsAuthorizationGrant};

pub(super) const RETRY_SCHEDULE: BackoffSchedule = BackoffSchedule {
    interval: Duration::from_millis(100),
    cap: Duration::from_secs(5),
};

#[derive(Debug, Clone, Default)]
pub(crate) struct NatsAuthorizationHealthReader {
    state: Arc<Mutex<NatsAuthorizationHealthState>>,
    #[cfg(test)]
    changes: tokio::sync::watch::Sender<u64>,
}

impl NatsAuthorizationHealthReader {
    #[must_use]
    pub(crate) fn snapshot(&self) -> ployz_sdk_types::ControlNatsAuthorizationHealth {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ployz_sdk_types::ControlNatsAuthorizationHealth {
            consecutive_failures: state.consecutive_failures,
            last_failure: state.last_failure.clone(),
            next_expiry_at_unix_seconds: state.next_expiry_at_unix_seconds,
        }
    }

    pub(super) fn record_success(&self, next_expiry_at_unix_seconds: Option<u64>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.consecutive_failures = 0;
        state.last_failure = None;
        state.next_expiry_at_unix_seconds = next_expiry_at_unix_seconds;
        #[cfg(test)]
        self.changes
            .send_modify(|revision| *revision = revision.saturating_add(1));
    }

    pub(super) fn record_failure(&self, failure: impl std::fmt::Display) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        state.last_failure = Some(failure.to_string());
        #[cfg(test)]
        self.changes
            .send_modify(|revision| *revision = revision.saturating_add(1));
    }

    #[cfg(test)]
    pub(super) fn subscribe_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }
}

#[derive(Debug, Default)]
struct NatsAuthorizationHealthState {
    consecutive_failures: u64,
    last_failure: Option<String>,
    next_expiry_at_unix_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AuthorizationWake {
    delay: Duration,
}

impl AuthorizationWake {
    pub(super) const fn wait_duration(self) -> Duration {
        self.delay
    }
}

pub(super) async fn schedule_after_success(
    store: &NatsAuthorizationStore,
    now_unix_seconds: u64,
    health: &NatsAuthorizationHealthReader,
    retry_delay: Duration,
) -> (Option<AuthorizationWake>, Duration) {
    match store.list().await {
        Ok(grants) => {
            let next_expiry_at_unix_seconds = next_active_expiry(&grants, now_unix_seconds);
            health.record_success(next_expiry_at_unix_seconds);
            (
                next_expiry_at_unix_seconds.map(|expires_at_unix_seconds| AuthorizationWake {
                    delay: Duration::from_secs(
                        expires_at_unix_seconds.saturating_sub(now_unix_seconds),
                    ),
                }),
                RETRY_SCHEDULE.interval,
            )
        }
        Err(error) => {
            health.record_failure(error);
            retry_after_failure(retry_delay)
        }
    }
}

pub(super) fn retry_after_failure(retry_delay: Duration) -> (Option<AuthorizationWake>, Duration) {
    (
        Some(AuthorizationWake { delay: retry_delay }),
        RETRY_SCHEDULE.next_after_failure(retry_delay),
    )
}

fn next_active_expiry(grants: &[NatsAuthorizationGrant], now_unix_seconds: u64) -> Option<u64> {
    grants
        .iter()
        .filter_map(|grant| match grant {
            NatsAuthorizationGrant::Credential(CredentialGrant {
                role: CredentialRole::BuildExecutor { expires_at, .. },
                ..
            }) if expires_at.unix_seconds() > now_unix_seconds => Some(expires_at.unix_seconds()),
            NatsAuthorizationGrant::Credential(CredentialGrant {
                role: CredentialRole::Operator,
                ..
            })
            | NatsAuthorizationGrant::Credential(CredentialGrant {
                role: CredentialRole::BuildExecutor { .. },
                ..
            })
            | NatsAuthorizationGrant::Internal { .. } => None,
        })
        .min()
}
