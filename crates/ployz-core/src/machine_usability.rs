//! The Machine Usability View: the one place the placement, serving, and
//! cleanup eligibility rules live. Pure functions over self-reported machine
//! state — consumers (deploy facts, gateway, DNS, machine API) fold these
//! rules over data they already read; nothing here runs, caches, or stores.

use crate::state::MachineLifecycle;
use std::time::Duration;

/// How often each machine publishes its own reality (container snapshot,
/// public ip, role status) into the observation KV.
pub const OBSERVATION_PUBLISH_INTERVAL: Duration = Duration::from_secs(30);

/// The single freshness rule: an observation older than three publish beats
/// is stale everywhere — gateway serving, DNS, machine API, and placement
/// all use this constant.
pub const OBSERVATION_STALE_AFTER: Duration = Duration::from_secs(90);

/// Why a machine is not currently usable for a purpose. Only reasons with a
/// real signal exist; the glossary's remaining reasons land with theirs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineUsabilityReason {
    Draining,
    StaleObservation,
    NoRuntimeObservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "verdict", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineUsabilityVerdict {
    Usable,
    Unusable { reason: MachineUsabilityReason },
}

impl MachineUsabilityVerdict {
    #[must_use]
    pub const fn is_usable(&self) -> bool {
        matches!(self, Self::Usable)
    }
}

/// The three eligibility dimensions for one machine. Placement follows
/// operator intent and freshness; serving follows freshness only (a draining
/// machine keeps serving until the next deploy empties it); cleanup requires
/// only that the machine has ever reported (it must accept removals to
/// empty).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineUsability {
    pub placement: MachineUsabilityVerdict,
    pub serving: MachineUsabilityVerdict,
    pub cleanup: MachineUsabilityVerdict,
}

/// How fresh the machine's most recent self-report is, computed by the
/// caller from observation timestamps and [`OBSERVATION_STALE_AFTER`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationFreshness {
    Fresh,
    Stale,
    Missing,
}

impl ObservationFreshness {
    /// Classifies an observation age against the shared staleness rule.
    /// `None` means the machine has never reported.
    #[must_use]
    pub fn from_age(age: Option<Duration>) -> Self {
        match age {
            None => Self::Missing,
            Some(age) if age > OBSERVATION_STALE_AFTER => Self::Stale,
            Some(_) => Self::Fresh,
        }
    }
}

#[must_use]
pub fn machine_usability(
    lifecycle: MachineLifecycle,
    freshness: ObservationFreshness,
) -> MachineUsability {
    let freshness_verdict = match freshness {
        ObservationFreshness::Fresh => MachineUsabilityVerdict::Usable,
        ObservationFreshness::Stale => MachineUsabilityVerdict::Unusable {
            reason: MachineUsabilityReason::StaleObservation,
        },
        ObservationFreshness::Missing => MachineUsabilityVerdict::Unusable {
            reason: MachineUsabilityReason::NoRuntimeObservation,
        },
    };
    let placement = match lifecycle {
        MachineLifecycle::Draining => MachineUsabilityVerdict::Unusable {
            reason: MachineUsabilityReason::Draining,
        },
        MachineLifecycle::Active => freshness_verdict,
    };
    let cleanup = match freshness {
        ObservationFreshness::Missing => MachineUsabilityVerdict::Unusable {
            reason: MachineUsabilityReason::NoRuntimeObservation,
        },
        ObservationFreshness::Fresh | ObservationFreshness::Stale => {
            MachineUsabilityVerdict::Usable
        }
    };

    MachineUsability {
        placement,
        serving: freshness_verdict,
        cleanup,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usability_table_covers_lifecycle_and_freshness() {
        let usable = MachineUsabilityVerdict::Usable;
        let draining = MachineUsabilityVerdict::Unusable {
            reason: MachineUsabilityReason::Draining,
        };
        let stale = MachineUsabilityVerdict::Unusable {
            reason: MachineUsabilityReason::StaleObservation,
        };
        let missing = MachineUsabilityVerdict::Unusable {
            reason: MachineUsabilityReason::NoRuntimeObservation,
        };

        let cases = [
            (
                MachineLifecycle::Active,
                ObservationFreshness::Fresh,
                (usable, usable, usable),
            ),
            (
                MachineLifecycle::Active,
                ObservationFreshness::Stale,
                (stale, stale, usable),
            ),
            (
                MachineLifecycle::Active,
                ObservationFreshness::Missing,
                (missing, missing, missing),
            ),
            (
                MachineLifecycle::Draining,
                ObservationFreshness::Fresh,
                (draining, usable, usable),
            ),
            (
                MachineLifecycle::Draining,
                ObservationFreshness::Stale,
                (draining, stale, usable),
            ),
            (
                MachineLifecycle::Draining,
                ObservationFreshness::Missing,
                (draining, missing, missing),
            ),
        ];
        for (lifecycle, freshness, (placement, serving, cleanup)) in cases {
            let usability = machine_usability(lifecycle, freshness);
            assert_eq!(
                usability.placement, placement,
                "{lifecycle:?}/{freshness:?}"
            );
            assert_eq!(usability.serving, serving, "{lifecycle:?}/{freshness:?}");
            assert_eq!(usability.cleanup, cleanup, "{lifecycle:?}/{freshness:?}");
        }
    }

    #[test]
    fn freshness_classifies_against_the_shared_rule() {
        assert_eq!(
            ObservationFreshness::from_age(None),
            ObservationFreshness::Missing
        );
        assert_eq!(
            ObservationFreshness::from_age(Some(OBSERVATION_STALE_AFTER)),
            ObservationFreshness::Fresh
        );
        assert_eq!(
            ObservationFreshness::from_age(Some(OBSERVATION_STALE_AFTER + Duration::from_secs(1))),
            ObservationFreshness::Stale
        );
    }
}
