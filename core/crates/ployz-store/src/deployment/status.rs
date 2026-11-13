//! The SQL a Deployment's status takes in a predicate. Each set of statuses is built
//! from [`DeploymentStatus`] here, once, so the Store's queries name a set and never
//! spell its members.

use std::sync::LazyLock;

use super::{DeploymentStatus, now};

impl DeploymentStatus {
    /// Every status.
    pub const ALL: [Self; 9] = [
        Self::WaitingForCi,
        Self::Queued,
        Self::Superseded,
        Self::Running,
        Self::Applied,
        Self::Failed,
        Self::Unknown,
        Self::Cancelling,
        Self::Cancelled,
    ];

    /// The name the Store keeps in a row and a query spells: its serde name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WaitingForCi => "waiting_for_ci",
            Self::Queued => "queued",
            Self::Superseded => "superseded",
            Self::Running => "running",
            Self::Applied => "applied",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::Cancelling => "cancelling",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether a runner holds it: running or cancelling.
    #[must_use]
    pub const fn claimed(self) -> bool {
        matches!(self, Self::Running | Self::Cancelling)
    }
}

/// `status IN (...)` over the statuses of [`DeploymentStatus::ALL`] that `member` keeps.
fn status_in(member: impl Fn(DeploymentStatus) -> bool) -> String {
    let names: Vec<String> = DeploymentStatus::ALL
        .into_iter()
        .filter(|status| member(*status))
        .map(|status| format!("'{}'", status.as_str()))
        .collect();
    format!("status IN ({})", names.join(", "))
}

/// A Deployment that may still run: queued, running or cancelling. Spelled as the
/// `config_deployment_in_flight` index's predicate is, so SQLite's planner takes the
/// index; frozen migration 0035 carries the same text.
pub(crate) static IN_FLIGHT: LazyLock<String> =
    LazyLock::new(|| status_in(DeploymentStatus::in_flight));

/// A Deployment a runner holds: running or cancelling.
pub(crate) static CLAIMED: LazyLock<String> =
    LazyLock::new(|| status_in(DeploymentStatus::claimed));

/// A Deployment that ended.
pub(crate) static ENDED: LazyLock<String> = LazyLock::new(|| status_in(DeploymentStatus::ended));

/// The rule every reading of "the Deployment history" shares: only numbered rows
/// count, in number order. A hold that never promoted (waiting, superseded or
/// cancelled while waiting) has no number and is not history. Postgres sorts
/// NULLs first under `DESC`, so the filter is never left to the sort order.
pub(crate) const NUMBERED: &str = "number IS NOT NULL";

/// A Deployment a newer admission did not replace before it started.
pub(crate) static NOT_SUPERSEDED: LazyLock<String> = LazyLock::new(|| {
    format!(
        "status NOT IN ('{}')",
        DeploymentStatus::Superseded.as_str()
    )
});

/// The rows a list of an Environment's Deployments shows: every numbered one, a
/// hold still waiting for CI, and a hold cancelled while waiting. A hold a newer
/// push replaced before CI answered is left out: it never got a number, and a
/// burst of them would push real Deployments off the first page.
pub(crate) static LISTED: LazyLock<String> = LazyLock::new(|| {
    format!(
        "({NUMBERED} OR status <> '{}')",
        DeploymentStatus::Superseded.as_str()
    )
});

/// SQL selecting Deployments that may still run: queued, or claimed by a runner
/// whose lease holds. The inverse of [`lapsed_sql`] for claimed ones. The status set
/// comes first, spelled as [`IN_FLIGHT`] is, so SQLite's planner takes the index.
pub(crate) fn in_flight_sql() -> String {
    format!(
        "({} AND (status = '{}' OR lease > {}))",
        IN_FLIGHT.as_str(),
        DeploymentStatus::Queued.as_str(),
        now()
    )
}

/// SQL selecting Deployments whose runner is gone: claimed, and its runner lease
/// lapsed ([`super::lapsed`]).
pub(crate) fn lapsed_sql() -> String {
    format!("({} AND lease <= {})", CLAIMED.as_str(), now())
}
