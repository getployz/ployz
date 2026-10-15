use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{ContainerId, DeploymentLogId, QualifiedService};

/// Docker label carrying the deployment that created a Container.
pub const DEPLOYMENT_ID_LABEL: &str = "ployz.deployment.id";

/// Which Containers of one Service take traffic, stored at [`ServingPointer::key`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServingPointer {
    pub deploy: DeploymentLogId,
    pub containers: Vec<ContainerId>,
    /// Deploy that served before this pointer, routed while a pointed row is still missing.
    pub previous: Option<DeploymentLogId>,
    /// Containers this pointer superseded. While a pointed row is missing they
    /// serve with `previous`, which matters for old copies without a deploy label.
    pub retiring: Vec<ContainerId>,
    /// Deploy that stopped Containers in `retiring` and has not decided yet.
    /// While it holds, `retiring` is neither retired nor pruned: a failed
    /// Deploy restarts those Containers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deciding: Option<Hold>,
}

/// An undecided Deploy's claim on `retiring`, honoured until a wall-clock
/// time the Deploy chose well past its own end.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Hold {
    pub deploy: DeploymentLogId,
    pub until_unix_seconds: u64,
}

impl Hold {
    #[must_use]
    pub fn lasting(deploy: DeploymentLogId, lasting: Duration) -> Self {
        let until = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .saturating_add(lasting);
        Self {
            deploy,
            until_unix_seconds: until.as_secs(),
        }
    }

    /// An expiry past the last representable time counts as lapsed.
    #[must_use]
    pub fn holds_at(&self, now: SystemTime) -> bool {
        UNIX_EPOCH
            .checked_add(Duration::from_secs(self.until_unix_seconds))
            .is_some_and(|until| now < until)
    }
}

impl ServingPointer {
    /// Whether an undecided Deploy still holds `retiring` against retirement and pruning.
    #[must_use]
    pub fn is_held(&self) -> bool {
        self.deciding
            .as_ref()
            .is_some_and(|hold| hold.holds_at(SystemTime::now()))
    }

    /// Cluster table key `serving/<namespace>/<service>`.
    #[must_use]
    pub fn key(service: &QualifiedService) -> String {
        format!("serving/{service}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hold_past_the_last_representable_time_has_lapsed() {
        let hold = Hold {
            deploy: DeploymentLogId::parse("run").unwrap(),
            until_unix_seconds: u64::MAX,
        };

        assert!(!hold.holds_at(SystemTime::now()));
    }
}
