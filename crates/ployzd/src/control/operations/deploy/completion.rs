use ployz_core::deploy::{
    DeployPlan, DeployPlanStepRef, DeployServicePlacement, DeployServicePlan,
};
use ployz_core::operation::DeployServiceResult;

use super::types::{DeployServiceExecutionCommand, ServingIntentDisposition};

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

pub(super) fn service_result(
    command: &DeployServiceExecutionCommand,
    service: &DeployServicePlan,
    has_cleanup: bool,
) -> DeployServiceResult {
    if matches!(command.serving_intent, ServingIntentDisposition::Unchanged)
        && !has_cleanup
        && service.pre_start.is_none()
        && service
            .work
            .steps()
            .all(|step| matches!(step, DeployPlanStepRef::UseExisting { .. }))
    {
        DeployServiceResult::Unchanged {
            service_id: service.service_id.clone(),
        }
    } else {
        DeployServiceResult::Completed {
            service_id: service.service_id.clone(),
        }
    }
}
