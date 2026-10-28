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
        if let DeployWarning::ObservationFailed {
            kind,
            machine_id,
            message,
            ..
        } = warning
            && let Some(note) = large_packets_note(snapshot, *kind, *machine_id)
        {
            message.push_str(&note);
        }
    }
    // Machines whose old copies may serve beside this Deploy's new ones; a gap
    // that only a removal opened starts no new copy there.
    let mut serving_beside = std::collections::BTreeSet::new();
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
        // Down and Unrecognized membership get no RPC, so their omission is a
        // Server this Entry did not read. Missing evidence is synthetic absence.
        let reason = match (failed, &machine.membership_evidence) {
            (true, _) | (false, Some(MembershipEvidence::Unrecognized { .. })) => {
                ObservationGapReason::Failed
            }
            (false, Some(MembershipEvidence::Down)) => ObservationGapReason::Down,
            (false, _) => continue,
        };
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
        let replacing =
            candidate || affected_existing(kind, machine_id, namespace, requested, &[], snapshot);
        if replacing && kind == ObservationKind::Container {
            serving_beside.insert(machine_id);
        }
        if replacing
            || removing
            || affected_existing(kind, machine_id, namespace, requested, retiring, snapshot)
        {
            *gap = Some(ObservationGap {
                machine_name: machine.machine.name.clone(),
                reason,
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
                machine_id,
                gap: Some(gap),
                ..
            }
            | DeployWarning::ObservationOmitted {
                kind: ObservationKind::Container,
                machine_id,
                gap: Some(gap),
            }) = warning
            else {
                return None;
            };
            if !serving_beside.contains(machine_id) {
                return None;
            }
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

/// [`crate::deploy::large_packets_note`] for the Server's failed `kind` observation,
/// which [`crate::deploy::unanswered_message`] adds when execution reports it again.
fn large_packets_note(
    snapshot: &DeploySnapshot,
    kind: ObservationKind,
    machine_id: MachineId,
) -> Option<String> {
    let failures = match kind {
        ObservationKind::Container => &snapshot.container_failures,
        ObservationKind::Volume => &snapshot.volume_snapshot.machine_failures,
    };
    let failure = failures
        .iter()
        .find(|failure| failure.machine_id == machine_id && failure.error.target_unanswered())?;
    let machine = snapshot
        .machines
        .iter()
        .find(|machine| machine.machine.id == machine_id)?;
    crate::deploy::large_packets_note(machine, &failure.error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::{
        LargePackets, MachineFailure, MachineObservation, MembershipObservation, PathObservation,
        RpcError, RpcErrorCode,
    };

    fn failed_message(path: Option<PathObservation>, error: RpcError) -> String {
        let machine = crate::global_fixtures::machine('a', "web-2");
        let mut observed = MachineObservation::new(machine.clone(), MembershipObservation::Up);
        observed.path = path;
        let snapshot = DeploySnapshot {
            machines: vec![observed],
            container_failures: vec![MachineFailure {
                machine_id: machine.id,
                error,
            }],
            ..Default::default()
        };
        let warnings = warnings(
            &Namespace::system(),
            &[],
            &[],
            &ServiceRemovals::default(),
            &snapshot,
        );
        let [DeployWarning::ObservationFailed { message, .. }] = warnings.as_slice() else {
            panic!("expected one observation failure, got {warnings:?}");
        };
        message.clone()
    }

    #[test]
    fn an_unanswered_observation_names_large_packets_the_entry_finds_dropped() {
        let timed_out = || {
            RpcError::unanswered_target(
                RpcErrorCode::Unavailable,
                "target Machine RPC timed out".into(),
            )
        };
        let dropped = Some(PathObservation {
            mtu: 1420,
            large_packets: LargePackets::Dropped,
            lowered: false,
        });
        assert_eq!(
            failed_message(dropped, timed_out()),
            "target Machine RPC timed out; large packets to web-2 are dropped on its mesh path from the Server this command entered by"
        );
        assert_eq!(
            failed_message(None, timed_out()),
            "target Machine RPC timed out",
            "a path the entry didn't find dropping large packets names no cause"
        );
        let refused = RpcError {
            code: RpcErrorCode::Conflict,
            message: "refused".into(),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        };
        assert_eq!(
            failed_message(dropped, refused),
            "refused",
            "a Server that answered keeps its own words"
        );
    }

    #[test]
    fn execution_words_an_unanswered_listing_as_planning_did() {
        let dropped = Some(PathObservation {
            mtu: 1420,
            large_packets: LargePackets::Dropped,
            lowered: false,
        });
        let error = RpcError::unanswered_target(
            RpcErrorCode::Unavailable,
            "target Machine RPC timed out".into(),
        );
        let mut machine = MachineObservation::new(
            crate::global_fixtures::machine('a', "web-2"),
            MembershipObservation::Suspect,
        );
        machine.path = dropped;
        // The notices dedupe by message, so the two read as one.
        assert_eq!(
            crate::deploy::unanswered_message(&machine, &error),
            failed_message(dropped, error)
        );
    }

    #[test]
    fn a_silent_server_is_a_listing_gap_only_when_its_silence_is_explained() {
        let dropped = Some(PathObservation {
            mtu: 1420,
            large_packets: LargePackets::Dropped,
            lowered: false,
        });
        let observed = |membership, path| {
            let mut machine =
                MachineObservation::new(crate::global_fixtures::machine('a', "web-2"), membership);
            machine.path = path;
            machine
        };
        let silent = || {
            RpcError::unanswered_target(
                RpcErrorCode::Unavailable,
                "target Machine RPC timed out".into(),
            )
        };
        let gap = crate::deploy::listing_gap;
        assert!(gap(
            &observed(MembershipObservation::Suspect, None),
            &silent()
        ));
        assert!(gap(
            &observed(MembershipObservation::Up, dropped),
            &silent()
        ));
        assert!(!gap(&observed(MembershipObservation::Up, None), &silent()));
        // A router that couldn't dial it sent only small packets.
        let undialed = silent().undialed_target();
        assert!(!gap(
            &observed(MembershipObservation::Up, dropped),
            &undialed
        ));
        assert!(gap(
            &observed(MembershipObservation::Suspect, dropped),
            &undialed
        ));
        assert_eq!(
            crate::deploy::unanswered_message(
                &observed(MembershipObservation::Up, dropped),
                &undialed
            ),
            "target Machine RPC timed out",
            "nor does its notice blame large packets"
        );
        let refused = RpcError {
            code: RpcErrorCode::Conflict,
            message: "refused".into(),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        };
        assert!(!gap(
            &observed(MembershipObservation::Up, dropped),
            &refused
        ));
    }

    /// Only a listing left unanswered over a path the entry finds dropping large packets
    /// gives the plan that cause, for the notices execution words later (#3521).
    #[test]
    fn the_plan_keeps_the_servers_whose_listing_large_packets_explain() {
        use crate::deploy::planning::ingress_repairs::{ingress_intent, servers};

        let dropped = Some(PathObservation {
            mtu: 1420,
            large_packets: LargePackets::Dropped,
            lowered: false,
        });
        let silent = || {
            RpcError::unanswered_target(
                RpcErrorCode::Unavailable,
                "target Machine RPC timed out".into(),
            )
        };
        let machines = [
            crate::global_fixtures::machine('1', "web-1"),
            crate::global_fixtures::machine('2', "web-2"),
            crate::global_fixtures::machine('3', "web-3"),
            crate::global_fixtures::machine('4', "web-4"),
        ];
        let mut snapshot = servers(&machines, Vec::new());
        // web-2 is silent over a dropping path; web-3 too, but the router never dialed
        // it; web-4's path drops, but it answered.
        for (observed, membership) in snapshot.machines.iter_mut().skip(1).zip([
            MembershipObservation::Up,
            MembershipObservation::Suspect,
            MembershipObservation::Up,
        ]) {
            observed.membership = membership;
            observed.path = dropped;
        }
        snapshot.container_failures = vec![
            MachineFailure {
                machine_id: machines[1].id,
                error: silent(),
            },
            MachineFailure {
                machine_id: machines[2].id,
                error: silent().undialed_target(),
            },
        ];

        let plan = crate::deploy::plan_deploy(&ingress_intent(), &snapshot).expect("a plan");

        assert_eq!(
            plan.dropped_large_packets(),
            &std::collections::BTreeSet::from([machines[1].id])
        );
    }
}
