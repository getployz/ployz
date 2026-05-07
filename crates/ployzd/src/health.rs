use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ComponentHealth {
    pub updated_at_unix_secs: u64,
    pub state: ComponentHealthState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum ComponentHealthState {
    Healthy,
    Stale {
        stale_since_unix_secs: u64,
        consecutive_failures: u64,
        last_error: String,
    },
}

impl ComponentHealth {
    pub(crate) fn healthy(updated_at_unix_secs: u64) -> Self {
        Self {
            updated_at_unix_secs,
            state: ComponentHealthState::Healthy,
        }
    }

    pub(crate) fn stale(
        updated_at_unix_secs: u64,
        previous: Option<&Self>,
        last_error: impl Into<String>,
    ) -> Self {
        let (stale_since_unix_secs, consecutive_failures) = match previous {
            Some(Self {
                state:
                    ComponentHealthState::Stale {
                        stale_since_unix_secs,
                        consecutive_failures,
                        ..
                    },
                ..
            }) => (
                *stale_since_unix_secs,
                consecutive_failures.saturating_add(1),
            ),
            _ => (updated_at_unix_secs, 1),
        };
        Self {
            updated_at_unix_secs,
            state: ComponentHealthState::Stale {
                stale_since_unix_secs,
                consecutive_failures,
                last_error: last_error.into(),
            },
        }
    }

    #[must_use]
    pub(crate) fn is_healthy(&self) -> bool {
        matches!(self.state, ComponentHealthState::Healthy)
    }

    #[must_use]
    pub(crate) fn stale_since_unix_secs(&self) -> Option<u64> {
        match self.state {
            ComponentHealthState::Healthy => None,
            ComponentHealthState::Stale {
                stale_since_unix_secs,
                ..
            } => Some(stale_since_unix_secs),
        }
    }

    #[must_use]
    pub(crate) fn consecutive_failures(&self) -> u64 {
        match self.state {
            ComponentHealthState::Healthy => 0,
            ComponentHealthState::Stale {
                consecutive_failures,
                ..
            } => consecutive_failures,
        }
    }

    #[must_use]
    pub(crate) fn last_error(&self) -> Option<&str> {
        match self.state {
            ComponentHealthState::Healthy => None,
            ComponentHealthState::Stale { ref last_error, .. } => Some(last_error),
        }
    }
}
