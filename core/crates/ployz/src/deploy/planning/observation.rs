//! Qualify existing snapshot warnings against bound work, never the whole desired target.

use super::DeploySnapshot;
use ployz_core::{
    DeployWarning, MachineId, MembershipEvidence, Namespace, ObservationGap, ObservationGapReason,
    ObservationKind, QualifiedService, RequestedServiceSpec, ServicePlacementEligibility,
    ServiceRemovals,
};

/// `removals` names the Services this Deploy must take out wherever they run, and
/// `retiring` the ones it plans to, pruned Services included. A Machine it could
/// not check may hold a copy of one it never saw, so every such Machine is a gap:
/// the report names it, and the next full Deploy prunes that copy.
pub(super) fn warnings(
    namespace: &Namespace,
    requested: &[RequestedServiceSpec],
    retiring: &[QualifiedService],
    removals: &ServiceRemovals,
    snapshot: &DeploySnapshot,
) -> Vec<DeployWarning> {
    let mut warnings = super::super::observation_warnings(
        ObservationKind::Container,
        &snapshot.container_failures,
        &snapshot.container_omissions,
    );
    warnings.extend(snapshot.volume_snapshot.deploy_warnings());
    for warning in &mut warnings {
        let (kind, machine_id, gap, failed) = match warning {
            DeployWarning::ObservationFailed {
                kind,
                machine_id,
                gap,
                ..
            } => (*kind, *machine_id, gap, true),
            DeployWarning::ObservationOmitted {
                kind,
                machine_id,
                gap,
            } => (*kind, *machine_id, gap, false),
            DeployWarning::StorageHeadroom { .. }
            | DeployWarning::UnbudgetedDiskUsage
            | DeployWarning::StorageObservationUnknown { .. }
            | DeployWarning::IngressHostname { .. }
            | DeployWarning::ObserverRelativeHostnameConflict
            | DeployWarning::SkippedDependencyHealth { .. }
            | DeployWarning::ImagePortMismatch { .. }
            | DeployWarning::LeftRunning { .. }
            | DeployWarning::MixedAfterReturn { .. }
            | DeployWarning::NotInStore { .. } => continue,
        };
        let Some(machine) = snapshot
            .machines
            .iter()
            .find(|machine| machine.machine.id == machine_id)
        else {
            continue;
        };
        if !failed && machine.membership_evidence != Some(MembershipEvidence::Down) {
            continue;
        }
        let candidate = requested.iter().any(|spec| {
            relevant_kind(kind, spec)
                && !matches!(
                    spec.placement_eligibility_in_namespace(
                        namespace,
                        &machine.machine,
                        machine.storage.as_ref()
                    ),
                    ServicePlacementEligibility::Ineligible(_)
                )
        });
        let removing =
            kind == ObservationKind::Container && (!removals.is_empty() || !retiring.is_empty());
        if candidate
            || removing
            || affected_existing(kind, machine_id, namespace, requested, retiring, snapshot)
        {
            *gap = Some(ObservationGap {
                machine_name: machine.machine.name.clone(),
                reason: if failed {
                    ObservationGapReason::Failed
                } else {
                    ObservationGapReason::Down
                },
            });
        }
    }
    // Its old copies were never observed, so no cleanup report reaches them;
    // they keep running until the next Deploy, and serve beside the new copies
    // unless their Machine restarts and its boot pass confirms the new ones.
    let away: Vec<DeployWarning> = warnings
        .iter()
        .filter_map(|warning| {
            let (DeployWarning::ObservationFailed {
                kind: ObservationKind::Container,
                gap: Some(gap),
                ..
            }
            | DeployWarning::ObservationOmitted {
                kind: ObservationKind::Container,
                gap: Some(gap),
                ..
            }) = warning
            else {
                return None;
            };
            Some(DeployWarning::MixedAfterReturn {
                machine: gap.machine_name.to_string(),
            })
        })
        .collect();
    warnings.extend(away);
    warnings
}

fn relevant_kind(kind: ObservationKind, spec: &RequestedServiceSpec) -> bool {
    match kind {
        ObservationKind::Container => true,
        ObservationKind::Volume => spec
            .volume_graph()
            .mounted_volumes()
            .any(|volume| volume.source.docker_volume_name().is_some()),
    }
}

fn affected_existing(
    kind: ObservationKind,
    machine: MachineId,
    namespace: &Namespace,
    requested: &[RequestedServiceSpec],
    retiring: &[QualifiedService],
    snapshot: &DeploySnapshot,
) -> bool {
    snapshot.containers.iter().any(|container| {
        container.machine_id == machine
            && container.namespace == *namespace
            && (requested
                .iter()
                .any(|spec| spec.name == container.resolved_spec.name)
                || retiring
                    .iter()
                    .any(|service| service.name == container.resolved_spec.name))
            && match kind {
                ObservationKind::Container => true,
                ObservationKind::Volume => container
                    .resolved_spec
                    .volume_graph()
                    .mounted_volumes()
                    .any(|volume| volume.source.docker_volume_name().is_some()),
            }
    })
}
