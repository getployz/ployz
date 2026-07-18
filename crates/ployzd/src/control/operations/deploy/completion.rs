use ployz_core::deploy::{DeployPlan, DeployServicePlacement};
use ployz_core::operation::{DeployCompletionOutcome, DeployImageCleanup};

use super::DeployCleanupResult;

pub(super) fn deploy_completion_outcome(
    cleanup: &[DeployCleanupResult],
    images: &[DeployImageCleanup],
    global_deferrals: bool,
) -> DeployCompletionOutcome {
    if global_deferrals {
        DeployCompletionOutcome::CompletedWithWarnings
    } else {
        DeployCleanupResult::completion_outcome(cleanup, images)
    }
}

pub(super) fn plan_has_global_deferrals(plan: &DeployPlan) -> bool {
    plan.phases
        .iter()
        .flat_map(|phase| &phase.services)
        .any(|service| {
            matches!(
                &service.placement,
                DeployServicePlacement::Global { deferred, .. } if !deferred.is_empty()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::deploy::{DeployPhasePlan, DeployServicePlan};
    use ployz_core::ids::{MachineId, NamespaceId, NamespaceRevisionId, ServiceId};
    use ployz_core::operation::UnusableMachine;

    fn machine(value: &str) -> MachineId {
        MachineId::try_new(value).expect("machine id")
    }

    #[test]
    fn only_global_deferrals_add_a_completion_warning() {
        let mut plan = DeployPlan {
            namespace_id: NamespaceId::try_new("default").expect("namespace"),
            namespace_revision_id: NamespaceRevisionId::try_new("revision").expect("revision"),
            phases: vec![DeployPhasePlan {
                services: vec![DeployServicePlan {
                    service_id: ServiceId::try_new("api").expect("service"),
                    placement: DeployServicePlacement::Global {
                        candidates: vec![machine("machine_a")],
                        selected: Vec::new(),
                        deferred: Vec::new(),
                        draining: vec![machine("machine_a")],
                    },
                    steps: Vec::new(),
                    pre_start: None,
                }],
            }],
            volume_pin_commits: Vec::new(),
            volume_ensures: Vec::new(),
            cleanup_actions: Vec::new(),
        };
        assert!(!plan_has_global_deferrals(&plan));

        let [phase] = plan.phases.as_mut_slice() else {
            panic!("one phase")
        };
        let [service] = phase.services.as_mut_slice() else {
            panic!("one service")
        };
        let DeployServicePlacement::Global { deferred, .. } = &mut service.placement else {
            panic!("global placement")
        };
        deferred.push(UnusableMachine {
            machine_id: machine("machine_a"),
            reason: ployz_core::machine::MachineUsabilityReason::FactsUnavailable,
        });
        assert!(plan_has_global_deferrals(&plan));
        assert_eq!(
            deploy_completion_outcome(&[], &[], plan_has_global_deferrals(&plan)),
            DeployCompletionOutcome::CompletedWithWarnings
        );
    }
}
