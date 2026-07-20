//! Shared admission for operation-owned machine-local execution.

use std::collections::BTreeMap;

use ployz_core::ids::MachineId;
use ployz_core::image::OciPlatform;
use ployz_core::machine::{
    DataplaneProjectionAdmissionFailure, DataplaneUnavailableReason, MachineUsabilityReason,
    placement_rejection, validate_declared_local_machine,
};
use ployz_core::network::{DataplaneProjection, DataplaneProjectionMember, MachineDataplaneStatus};
use ployz_core::operation::{FailureMessage, UnusableMachine};

use crate::control::role_client::machine::MachinePlacementFacts;
use crate::roles::machine::protocol::MachineBuildCapability;

pub(crate) struct AdmittedLocalExecutionMachine<'a> {
    pub(crate) machine_id: &'a MachineId,
    pub(crate) platform: &'a OciPlatform,
    pub(crate) build: &'a MachineBuildCapability,
    pub(crate) member: &'a DataplaneProjectionMember,
    pub(crate) status: &'a MachineDataplaneStatus,
}

pub(crate) fn classify_local_execution_admission<'a>(
    placement_facts: &'a [MachinePlacementFacts],
    projection: &'a DataplaneProjection,
    dataplane_statuses: &'a [(MachineId, Result<MachineDataplaneStatus, FailureMessage>)],
) -> (Vec<AdmittedLocalExecutionMachine<'a>>, Vec<UnusableMachine>) {
    let mut admitted = Vec::new();
    let mut unusable = BTreeMap::new();
    for candidate in placement_facts {
        match admit_local_execution_machine(candidate, projection, dataplane_statuses) {
            Ok(candidate) => admitted.push(candidate),
            Err(reason) => {
                unusable.insert(candidate.machine_id.clone(), reason);
            }
        }
    }
    admitted.sort_by(|left, right| left.machine_id.cmp(right.machine_id));
    let unusable = unusable
        .into_iter()
        .map(|(machine_id, reason)| UnusableMachine { machine_id, reason })
        .collect();
    (admitted, unusable)
}

fn admit_local_execution_machine<'a>(
    candidate: &'a MachinePlacementFacts,
    projection: &'a DataplaneProjection,
    dataplane_statuses: &'a [(MachineId, Result<MachineDataplaneStatus, FailureMessage>)],
) -> Result<AdmittedLocalExecutionMachine<'a>, MachineUsabilityReason> {
    if let Some(reason) = placement_rejection(candidate.lifecycle) {
        return Err(reason);
    }
    let Some(facts) = &candidate.answer else {
        return Err(MachineUsabilityReason::FactsUnavailable);
    };
    let (member, status) =
        declared_local_dataplane_candidate(&candidate.machine_id, projection, dataplane_statuses)?;
    Ok(AdmittedLocalExecutionMachine {
        machine_id: &candidate.machine_id,
        platform: &facts.platform,
        build: &facts.build,
        member,
        status,
    })
}

fn declared_local_dataplane_candidate<'a>(
    machine_id: &MachineId,
    projection: &'a DataplaneProjection,
    dataplane_statuses: &'a [(MachineId, Result<MachineDataplaneStatus, FailureMessage>)],
) -> Result<(&'a DataplaneProjectionMember, &'a MachineDataplaneStatus), MachineUsabilityReason> {
    let Some(member) = projection
        .declared_members()
        .iter()
        .find(|member| &member.machine_id == machine_id)
    else {
        return Err(dataplane_unavailable(
            DataplaneUnavailableReason::NotDeclared,
        ));
    };
    let Some((_, testimony)) = dataplane_statuses
        .iter()
        .find(|(candidate_id, _)| candidate_id == machine_id)
    else {
        return Err(dataplane_unavailable(
            DataplaneUnavailableReason::TestimonyMissing,
        ));
    };
    let status = testimony.as_ref().map_err(|message| {
        dataplane_admission_failure(DataplaneProjectionAdmissionFailure::NoAnswer {
            message: message.clone(),
        })
    })?;
    if let Some(failure) = validate_declared_local_machine(projection, member, status) {
        return Err(dataplane_admission_failure(failure));
    }
    Ok((member, status))
}

pub(crate) fn dataplane_admission_failure(
    failure: DataplaneProjectionAdmissionFailure,
) -> MachineUsabilityReason {
    dataplane_unavailable(DataplaneUnavailableReason::Admission { failure })
}

fn dataplane_unavailable(reason: DataplaneUnavailableReason) -> MachineUsabilityReason {
    MachineUsabilityReason::DataplaneUnavailable { reason }
}
