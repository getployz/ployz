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
}
