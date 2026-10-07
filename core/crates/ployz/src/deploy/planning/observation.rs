//! Qualify existing snapshot warnings against bound work, never the whole desired target.

use super::DeploySnapshot;
use ployz_core::{
    DeployWarning, MachineId, MembershipEvidence, Namespace, ObservationGap, ObservationGapReason,
    ObservationKind, QualifiedService, RequestedServiceSpec, ServicePlacementEligibility,
};

pub(super) fn warnings(
    namespace: &Namespace,
    requested: &[RequestedServiceSpec],
    retiring: &[QualifiedService],
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
            | DeployWarning::SkippedDependencyHealth { .. } => continue,
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
        if candidate
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
