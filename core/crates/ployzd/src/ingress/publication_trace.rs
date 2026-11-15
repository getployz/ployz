//! The owner facts the public trace page reports, derived from the observed Service Containers.

use ployz_core::{DeploymentLogId, MachineId, QualifiedService, ServiceContainer, labels};
use serde::Serialize;
use std::{collections::BTreeMap, num::NonZeroUsize};

/// The owner facts the trace page shows, independent of health and endpoint eligibility.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct PublicationTrace {
    /// Deployment attempt of the owner's newest Service Container that is In, or when none
    /// is, the newest held or pending one, or else the newest of any; when it carries a
    /// valid one. So it changes only with the In set, which changes only with a route.
    pub(super) deploy: Option<DeploymentLogId>,
    /// Count of the owner's observed Service Containers per Machine, in any runtime state,
    /// leaving out copies rotated Out: only their removal follows, and that removal changes
    /// no route, so counting them would keep a deploy's old copies on the page.
    pub(super) placement: BTreeMap<MachineId, NonZeroUsize>,
}

/// What the page reports about a Service no Container is observed for.
pub(super) static NO_TRACE: PublicationTrace = PublicationTrace {
    deploy: None,
    placement: BTreeMap::new(),
};

impl PublicationTrace {
    /// Trace facts for every Service, from one pass over its observed Service Containers.
    pub(super) fn by_service<'a>(
        containers: impl IntoIterator<Item = &'a ServiceContainer>,
    ) -> BTreeMap<QualifiedService, Self> {
        let mut grouped = BTreeMap::<
            QualifiedService,
            (BTreeMap<MachineId, NonZeroUsize>, &'a ServiceContainer),
        >::new();
        for container in containers {
            let observation = container.as_observation();
            let (placement, newest) = grouped
                .entry(observation.identity())
                .or_insert_with(|| (BTreeMap::new(), container));
            if !retired(container) {
                placement
                    .entry(observation.machine_id)
                    .and_modify(|count| *count = count.saturating_add(1))
                    .or_insert(NonZeroUsize::MIN);
            }
            if (
                naming_rank(container),
                container.as_observation().recency_key(),
            ) > (naming_rank(newest), newest.as_observation().recency_key())
            {
                *newest = container;
            }
        }
        grouped
            .into_iter()
            .map(|(service, (placement, newest))| {
                // Only the newest Container names the attempt; an older sibling's label would be stale.
                let deploy = newest
                    .as_observation()
                    .labels
                    .get(labels::DEPLOYMENT_ID)
                    .and_then(|label| DeploymentLogId::parse(label).ok());
                (service, Self { deploy, placement })
            })
            .collect()
    }
}

/// Which copies name the trace page's deploy first: In, then held or pending, then Out. A
/// copy leaving or joining without a route change then never changes the name.
fn naming_rank(container: &ServiceContainer) -> u8 {
    match container.as_observation().rotation() {
        ployz_core::CopyRotation::In => 2,
        ployz_core::CopyRotation::Held(_) | ployz_core::CopyRotation::Pending => 1,
        ployz_core::CopyRotation::Out => 0,
    }
}

/// Rotated Out for good: only its removal follows, which changes no route.
fn retired(container: &ServiceContainer) -> bool {
    matches!(
        container.as_observation().rotation(),
        ployz_core::CopyRotation::Out
    )
}
