//! Durable machine lifecycle and point-of-use placement eligibility.

use serde::{Deserialize, Serialize};

/// The durable operator-intent state of a current machine identity. Controls
/// placement policy; runtime readiness comes from observations, never from
/// lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MachineLifecycle {
    #[default]
    Active,
    Draining,
}

/// Why a machine is excluded from new workload placement for one operation.
/// Operator intent excludes durably; unavailable machine facts exclude only
/// the current operation runtime snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineUsabilityReason {
    PlatformMismatch {
        supported: crate::build::BuildPlatforms,
        reported: crate::image::OciPlatform,
    },
    Draining,
    FactsUnavailable,
    BuildUnavailable,
    StorageTestimonyNotReported,
    StorageUnprepared,
    StorageUnavailable {
        reason: super::StorageUnavailableReason,
    },
    StoragePoolMismatch {
        expected: crate::deploy::ZfsPoolName,
        reported: crate::deploy::ZfsPoolName,
    },
}

#[must_use]
pub fn placement_rejection(lifecycle: MachineLifecycle) -> Option<MachineUsabilityReason> {
    match lifecycle {
        MachineLifecycle::Active => None,
        MachineLifecycle::Draining => Some(MachineUsabilityReason::Draining),
    }
}

#[cfg(test)]
mod tests {
    use super::{MachineLifecycle, MachineUsabilityReason, placement_rejection};

    #[test]
    fn only_draining_excludes_placement() {
        assert_eq!(placement_rejection(MachineLifecycle::Active), None);
        assert_eq!(
            placement_rejection(MachineLifecycle::Draining),
            Some(MachineUsabilityReason::Draining)
        );
    }

    #[test]
    fn platform_mismatch_is_typed_attempt_evidence() {
        let reason = MachineUsabilityReason::PlatformMismatch {
            supported: crate::build::BuildPlatforms::try_new([
                crate::image::OciPlatform::try_new("linux", "arm64").expect("supported"),
                crate::image::OciPlatform::try_new("linux", "amd64").expect("supported"),
            ])
            .expect("distinct platforms"),
            reported: crate::image::OciPlatform::try_new("linux", "amd64").expect("reported"),
        };
        assert_eq!(
            serde_json::to_value(reason).expect("json"),
            serde_json::json!({"kind":"platform_mismatch","supported":[{"os":"linux","architecture":"amd64"},{"os":"linux","architecture":"arm64"}],"reported":{"os":"linux","architecture":"amd64"}})
        );
    }

    #[test]
    fn build_unavailable_is_typed_attempt_evidence() {
        assert_eq!(
            serde_json::to_value(MachineUsabilityReason::BuildUnavailable).expect("json"),
            serde_json::json!({"kind":"build_unavailable"})
        );
    }
}
