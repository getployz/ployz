use super::{
    ContainerId, ContainerRetentionCount, MachineId, ManagedContainerIdentity, VolumeName,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployCleanupContainer {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub identity: ManagedContainerIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedCleanupCandidate {
    pub target: DeployCleanupContainer,
    pub state: crate::machine::runtime::ContainerRuntimeState,
    pub named_volume_names: std::collections::BTreeSet<VolumeName>,
    pub created_at_unix_seconds: Option<i64>,
    pub observed_image_identity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployCleanupAction {
    RemoveContainer {
        target: DeployCleanupContainer,
    },
    RemoveContainerAndReclaimImage {
        target: DeployCleanupContainer,
        image_identity: crate::image::OciDigest,
    },
    RemoveContainerWithInvalidImageIdentity {
        target: DeployCleanupContainer,
        observed_identity: Option<String>,
    },
}

impl DeployCleanupAction {
    #[must_use]
    pub const fn target(&self) -> &DeployCleanupContainer {
        match self {
            Self::RemoveContainer { target }
            | Self::RemoveContainerAndReclaimImage { target, .. }
            | Self::RemoveContainerWithInvalidImageIdentity { target, .. } => target,
        }
    }
}

pub(super) fn plan_cleanup(
    mut candidates: Vec<ObservedCleanupCandidate>,
    selected_containers: &[&ContainerId],
    keep: Option<ContainerRetentionCount>,
) -> Vec<DeployCleanupAction> {
    candidates.retain(|candidate| !selected_containers.contains(&&candidate.target.container_id));
    if let Some(keep) = keep {
        retain_newest_stopped(&mut candidates, keep);
    }

    candidates
        .into_iter()
        .map(|candidate| {
            let ObservedCleanupCandidate {
                target,
                observed_image_identity,
                ..
            } = candidate;
            let Some(_) = keep else {
                return DeployCleanupAction::RemoveContainer { target };
            };
            match observed_image_identity {
                Some(observed_identity) => {
                    match crate::image::OciDigest::try_new(observed_identity.clone()) {
                        Ok(image_identity) => DeployCleanupAction::RemoveContainerAndReclaimImage {
                            target,
                            image_identity,
                        },
                        Err(_) => DeployCleanupAction::RemoveContainerWithInvalidImageIdentity {
                            target,
                            observed_identity: Some(observed_identity),
                        },
                    }
                }
                None => DeployCleanupAction::RemoveContainerWithInvalidImageIdentity {
                    target,
                    observed_identity: None,
                },
            }
        })
        .collect()
}

fn retain_newest_stopped(
    candidates: &mut Vec<ObservedCleanupCandidate>,
    keep: ContainerRetentionCount,
) {
    let mut retained = candidates
        .iter()
        .filter(|candidate| !candidate.state.is_running())
        .collect::<Vec<_>>();
    retained.sort_by(|left, right| {
        match (left.created_at_unix_seconds, right.created_at_unix_seconds) {
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (left, right) => right.cmp(&left),
        }
        .then_with(|| left.target.machine_id.cmp(&right.target.machine_id))
        .then_with(|| left.target.container_id.cmp(&right.target.container_id))
    });
    let retained = retained
        .into_iter()
        .take(usize::from(keep.get()))
        .map(|candidate| {
            (
                candidate.target.machine_id.clone(),
                candidate.target.container_id.clone(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    candidates.retain(|candidate| {
        candidate.state.is_running()
            || !retained.contains(&(
                candidate.target.machine_id.clone(),
                candidate.target.container_id.clone(),
            ))
    });
}
