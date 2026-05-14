use crate::deploy::participant::DeployParticipantClient;
use crate::deploy::plan::ResolvedPlan;
use crate::error::{DeployError, Error, Result};
use crate::model::{DeployEvent, DeployId, InstanceStatusRecord, MachineId, MachineMembership};
use futures_util::stream::{self, StreamExt, TryStreamExt};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const PARTICIPANT_INSPECT_CONCURRENCY: usize = 64;

struct InspectedParticipant {
    participant: MachineId,
    instances: Vec<InstanceStatusRecord>,
}

pub(super) struct ParticipantSet {
    machines: BTreeMap<MachineId, MachineMembership>,
    instances: Vec<InstanceStatusRecord>,
    namespace: ployz_spec::Namespace,
    deploy_id: DeployId,
}

impl ParticipantSet {
    pub(super) async fn inspect(
        participant_client: &dyn DeployParticipantClient,
        plan: &ResolvedPlan,
        local_machine_id: &MachineId,
        deploy_id: &DeployId,
    ) -> Result<(Self, Vec<DeployEvent>)> {
        let namespace = plan.namespace().clone();
        Self::inspect_participants(
            participant_client,
            namespace,
            plan.machine_map().clone(),
            plan.participants().clone(),
            local_machine_id,
            deploy_id,
        )
        .await
    }

    pub(super) async fn inspect_participants(
        participant_client: &dyn DeployParticipantClient,
        namespace: ployz_spec::Namespace,
        machine_map: HashMap<MachineId, MachineMembership>,
        participant_ids: BTreeSet<MachineId>,
        local_machine_id: &MachineId,
        deploy_id: &DeployId,
    ) -> Result<(Self, Vec<DeployEvent>)> {
        let sorted_participants = participant_ids.iter().cloned().collect::<Vec<_>>();
        let inspected: Vec<InspectedParticipant> = stream::iter(sorted_participants.into_iter())
            .map(|participant| {
                let machine = machine_map.get(&participant).cloned();
                let namespace = namespace.clone();
                let deploy_id = deploy_id.clone();
                async move {
                    let Some(machine) = machine else {
                        return Err(Error::Deploy(DeployError::ParticipantMissing {
                            machine_id: participant.as_str().to_string(),
                        }));
                    };
                    let instances = participant_client
                        .inspect_namespace(&machine, &namespace, &deploy_id, local_machine_id)
                        .await?;
                    Ok(InspectedParticipant {
                        participant,
                        instances,
                    })
                }
            })
            .buffer_unordered(PARTICIPANT_INSPECT_CONCURRENCY)
            .try_collect()
            .await?;

        let mut inspected = inspected;
        inspected.sort_by(|left, right| left.participant.as_str().cmp(right.participant.as_str()));

        let machines = participant_ids
            .iter()
            .map(|machine_id| {
                let machine = machine_map.get(machine_id).cloned().ok_or_else(|| {
                    Error::Deploy(DeployError::ParticipantMissing {
                        machine_id: machine_id.as_str().to_string(),
                    })
                })?;
                Ok((machine_id.clone(), machine))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let mut events = Vec::new();
        let mut instances = Vec::new();
        for inspected in inspected {
            let instance_count = inspected.instances.len();
            events.push(DeployEvent {
                step: "inspect".into(),
                message: format!(
                    "inspected '{}' ({} instances)",
                    inspected.participant, instance_count
                ),
            });
            instances.extend(inspected.instances);
        }
        instances.sort_by(|left, right| left.instance_id.as_str().cmp(right.instance_id.as_str()));

        Ok((
            Self {
                machines,
                instances,
                namespace,
                deploy_id: deploy_id.clone(),
            },
            events,
        ))
    }

    pub(super) fn get(&self, machine_id: &MachineId) -> Result<&MachineMembership> {
        self.machines.get(machine_id).ok_or_else(|| {
            Error::Deploy(DeployError::ParticipantMissing {
                machine_id: machine_id.as_str().to_string(),
            })
        })
    }

    pub(super) fn instances(&self) -> &[InstanceStatusRecord] {
        &self.instances
    }

    pub(super) fn namespace(&self) -> &ployz_spec::Namespace {
        &self.namespace
    }

    pub(super) fn deploy_id(&self) -> &DeployId {
        &self.deploy_id
    }
}
