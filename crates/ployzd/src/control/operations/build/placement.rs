use std::collections::BTreeMap;

use ployz_core::build::BuildPlatforms;
use ployz_core::ids::MachineId;
use ployz_core::image::OciPlatform;
use ployz_core::machine::{MachineUsabilityReason, placement_rejection};
use ployz_core::operation::{BuildOperationFailure, UnusableMachine};

use crate::control::role_client::machine::MachinePlacementFacts;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BuildPlacement {
    by_platform: BTreeMap<OciPlatform, MachineId>,
}

impl BuildPlacement {
    pub(crate) fn iter(&self) -> impl ExactSizeIterator<Item = (&OciPlatform, &MachineId)> {
        self.by_platform.iter()
    }
}

/// Resolves every requested platform before any machine work starts.
///
/// Facts are a fresh gather over the roster's known set. Input and output are
/// ordered, making the lowest eligible machine id the stable winner for a
/// platform regardless of RPC completion order.
pub(crate) fn place_build_platforms(
    platforms: &BuildPlatforms,
    facts: &[MachinePlacementFacts],
) -> Result<BuildPlacement, Box<BuildOperationFailure>> {
    let mut by_platform = BTreeMap::new();
    for platform in platforms.iter() {
        let mut unusable = Vec::new();
        let mut eligible = Vec::new();
        for candidate in facts {
            let reason = placement_rejection(candidate.lifecycle).or_else(|| {
                candidate.answer.as_ref().map_or(
                    Some(MachineUsabilityReason::FactsUnavailable),
                    |answer| {
                        (answer.platform != *platform).then(|| {
                            MachineUsabilityReason::PlatformMismatch {
                                required: platform.clone(),
                                reported: answer.platform.clone(),
                            }
                        })
                    },
                )
            });
            match reason {
                None => eligible.push(candidate.machine_id.clone()),
                Some(reason) => {
                    unusable.push(UnusableMachine {
                        machine_id: candidate.machine_id.clone(),
                        reason,
                    });
                }
            }
        }
        eligible.sort();
        unusable.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
        let Some(machine_id) = eligible.into_iter().next() else {
            return Err(Box::new(BuildOperationFailure::NoEligibleMachine {
                platform: platform.clone(),
                unusable,
            }));
        };
        by_platform.insert(platform.clone(), machine_id);
    }
    Ok(BuildPlacement { by_platform })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::machine::MachineLifecycle;

    use crate::control::role_client::machine::{
        MachinePlacementFacts, MachinePlacementFactsAnswer,
    };

    #[test]
    fn placement_is_deterministic_and_requires_fresh_native_platform_facts() {
        let amd64 = platform("linux", "amd64");
        let arm64 = platform("linux", "arm64");
        let platforms = BuildPlatforms::try_new([arm64.clone(), amd64.clone()])
            .expect("distinct build platforms");
        let facts = vec![
            silent("silent"),
            answering("z-amd64", amd64.clone()),
            answering("a-amd64", amd64.clone()),
            answering("arm64", arm64.clone()),
        ];

        let placement = place_build_platforms(&platforms, &facts).expect("complete placement");
        assert_eq!(
            placement
                .iter()
                .map(|(platform, machine)| (platform.clone(), machine.as_str()))
                .collect::<Vec<_>>(),
            vec![(amd64, "a-amd64"), (arm64, "arm64")]
        );
    }

    #[test]
    fn placement_fails_before_fanout_when_any_platform_has_no_candidate() {
        let amd64 = platform("linux", "amd64");
        let arm64 = platform("linux", "arm64");
        let platforms = BuildPlatforms::try_new([amd64.clone(), arm64.clone()])
            .expect("distinct build platforms");

        let failure =
            place_build_platforms(&platforms, &[answering("amd64", amd64), silent("silent")])
                .expect_err("arm64 has no fresh matching testimony");

        assert!(matches!(
            *failure,
            BuildOperationFailure::NoEligibleMachine { platform, unusable }
                if platform == arm64
                    && unusable.iter().any(|machine| {
                        machine.machine_id.as_str() == "silent"
                            && machine.reason == MachineUsabilityReason::FactsUnavailable
                    })
        ));
    }

    fn answering(machine: &str, platform: OciPlatform) -> MachinePlacementFacts {
        let machine_id = MachineId::try_new(machine).expect("machine id");
        MachinePlacementFacts {
            machine_id: machine_id.clone(),
            lifecycle: MachineLifecycle::Active,
            answer: Some(MachinePlacementFactsAnswer {
                containers:
                    ployz_core::machine::runtime::MachineContainerObservationSnapshot::try_new(
                        machine_id,
                        [],
                    )
                    .expect("empty snapshot"),
                platform,
                endpoints: None,
                storage: None,
            }),
        }
    }

    fn silent(machine: &str) -> MachinePlacementFacts {
        MachinePlacementFacts {
            machine_id: MachineId::try_new(machine).expect("machine id"),
            lifecycle: MachineLifecycle::Active,
            answer: None,
        }
    }

    fn platform(os: &str, architecture: &str) -> OciPlatform {
        OciPlatform::try_new(os, architecture).expect("OCI platform")
    }
}
