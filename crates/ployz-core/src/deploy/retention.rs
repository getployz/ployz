use super::{ContainerId, ContainerRetentionCount, MachineId, ManagedContainerIdentity};
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
    pub created_at_unix_seconds: Option<i64>,
    pub resolved_image_identity: Option<crate::image::OciDigest>,
    pub image_reclamation_eligible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployImageReclamation {
    Remove {
        target: DeployCleanupContainer,
        image_identity: crate::image::OciDigest,
    },
    MissingIdentity {
        target: DeployCleanupContainer,
    },
}

pub(super) fn plan_cleanup(
    mut candidates: Vec<ObservedCleanupCandidate>,
    selected_containers: &[&ContainerId],
    keep: Option<ContainerRetentionCount>,
) -> (Vec<DeployCleanupContainer>, Vec<DeployImageReclamation>) {
    candidates.retain(|candidate| !selected_containers.contains(&&candidate.target.container_id));
    if let Some(keep) = keep {
        retain_newest_stopped(&mut candidates, keep);
    }

    let image_reclamations = if keep.is_some() {
        candidates
            .iter()
            .filter(|candidate| candidate.image_reclamation_eligible)
            .map(|candidate| match &candidate.resolved_image_identity {
                Some(image_identity) => DeployImageReclamation::Remove {
                    target: candidate.target.clone(),
                    image_identity: image_identity.clone(),
                },
                None => DeployImageReclamation::MissingIdentity {
                    target: candidate.target.clone(),
                },
            })
            .collect()
    } else {
        Vec::new()
    };
    let cleanup = candidates
        .into_iter()
        .map(|candidate| candidate.target)
        .collect();
    (cleanup, image_reclamations)
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
