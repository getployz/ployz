use crate::deploy::plan::ResolvedPlan;
use crate::error::{DeployError, Error, Result};
use crate::model::{
    DeployChangeKind, DeployId, DeployPreview, DeployRecord, DeployState, InstanceStatusRecord,
    MachineId, ServiceRelease, ServiceReleaseRecord, ServiceRevisionRecord, ServiceRoutingPolicy,
};
#[cfg(test)]
use crate::model::{
    DeployStateGoal, DeployStateTransition, DeployTransitionEvidence, VolumeRecord,
};
#[cfg(test)]
use ployz_store_api::DeployCommit;
use ployz_types::spec::Namespace;
#[cfg(test)]
use ployz_types::time::now_unix_secs;
use std::collections::{BTreeSet, HashMap};

#[derive(Debug)]
pub(super) struct PreparedDeploy {
    deploy_id: DeployId,
    plan: ResolvedPlan,
    preview: DeployPreview,
    applying_record: DeployRecord,
    revisions: Vec<ServiceRevisionRecord>,
}

#[cfg(test)]
#[derive(Debug)]
pub(super) struct StartedCandidates {
    prepared: PreparedDeploy,
    started: HashMap<(String, String), InstanceStatusRecord>,
}

#[cfg(test)]
#[derive(Debug)]
pub(super) struct CommitPlan {
    commit: DeployCommit,
}

#[derive(Debug)]
pub(super) struct CleanupPlan {
    namespace: Namespace,
    participants: BTreeSet<MachineId>,
    active_instance_ids: BTreeSet<String>,
}

impl PreparedDeploy {
    pub(super) fn new(
        deploy_id: DeployId,
        started_at: u64,
        coordinator_machine_id: MachineId,
        plan: ResolvedPlan,
    ) -> Result<Self> {
        let preview = plan.to_preview(Vec::new());
        let applying_record = DeployRecord {
            deploy_id: deploy_id.clone(),
            namespace: plan.namespace().clone(),
            coordinator_machine_id: coordinator_machine_id.clone(),
            manifest_hash: plan.manifest_hash().to_string(),
            state: DeployState::Applying,
            started_at,
            committed_at: None,
            finished_at: None,
            summary_json: serde_json::to_string(&preview).map_err(|error| {
                Error::operation("deploy_apply", format!("serialize preview: {error}"))
            })?,
        };

        let mut revisions = Vec::new();
        for service in plan.services() {
            let Some(spec_json) = service.spec_json() else {
                continue;
            };
            let Some(revision_hash) = service.next_revision_hash() else {
                continue;
            };
            revisions.push(ServiceRevisionRecord {
                namespace: plan.namespace().clone(),
                service: service.service.clone(),
                revision_hash: revision_hash.to_string(),
                spec_json: spec_json.to_string(),
                created_by: coordinator_machine_id.clone(),
                created_at: started_at,
            });
        }

        Ok(Self {
            deploy_id,
            plan,
            preview,
            applying_record,
            revisions,
        })
    }

    pub(super) fn plan(&self) -> &ResolvedPlan {
        &self.plan
    }

    pub(super) fn deploy_id(&self) -> &DeployId {
        &self.deploy_id
    }

    pub(super) fn preview(&self) -> &DeployPreview {
        &self.preview
    }

    pub(super) fn applying_record(&self) -> &DeployRecord {
        &self.applying_record
    }

    pub(super) fn revisions(&self) -> &[ServiceRevisionRecord] {
        &self.revisions
    }

    #[cfg(test)]
    pub(super) fn into_started(
        self,
        started: HashMap<(String, String), InstanceStatusRecord>,
    ) -> StartedCandidates {
        StartedCandidates {
            prepared: self,
            started,
        }
    }
}

#[cfg(test)]
impl StartedCandidates {
    pub(super) fn into_commit_plan(
        self,
        removed_volumes: Vec<String>,
        volumes: Vec<VolumeRecord>,
    ) -> Result<CommitPlan> {
        let PreparedDeploy {
            deploy_id,
            plan,
            preview,
            applying_record,
            revisions,
        } = self.prepared;
        let committed_at = now_unix_secs();
        let releases = build_committed_releases(&plan, &self.started, &deploy_id, committed_at)?;
        let branch_lineage = plan.service_branch_lineage_records(&deploy_id, committed_at);
        let removed_services = plan
            .services()
            .iter()
            .filter(|service| service.action == DeployChangeKind::Remove)
            .map(|service| service.service.clone())
            .collect::<Vec<_>>();
        let summary_json = serde_json::to_string(&preview).map_err(|error| {
            Error::operation("deploy_apply", format!("serialize preview: {error}"))
        })?;
        let mut deploy = applying_record;
        deploy
            .apply_state_transition(DeployStateTransition {
                goal: DeployStateGoal::Commit { summary_json },
                evidence: DeployTransitionEvidence::DeployExecutor {
                    coordinator_machine_id: deploy.coordinator_machine_id.clone(),
                },
                at_unix_secs: committed_at,
            })
            .map_err(|error| {
                Error::Deploy(DeployError::StateTransitionRejected {
                    transition: "commit",
                    code: error.code(),
                    message: error.message().to_string(),
                })
            })?;
        Ok(CommitPlan {
            commit: DeployCommit {
                namespace: plan.namespace().clone(),
                revisions,
                removed_services,
                removed_volumes,
                branch_lineage,
                releases,
                volumes,
                deploy,
            },
        })
    }
}

#[cfg(test)]
impl CommitPlan {
    pub(super) fn commit(&self) -> &DeployCommit {
        &self.commit
    }
}

impl CleanupPlan {
    pub(super) fn new(
        namespace: Namespace,
        participants: BTreeSet<MachineId>,
        active_instance_ids: BTreeSet<String>,
    ) -> Self {
        Self {
            namespace,
            participants,
            active_instance_ids,
        }
    }

    pub(super) fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    pub(super) fn participants(&self) -> &BTreeSet<MachineId> {
        &self.participants
    }

    pub(super) fn active_instance_ids(&self) -> &BTreeSet<String> {
        &self.active_instance_ids
    }
}

#[cfg(test)]
fn build_committed_releases(
    plan: &ResolvedPlan,
    started: &HashMap<(String, String), InstanceStatusRecord>,
    deploy_id: &DeployId,
    updated_at: u64,
) -> Result<Vec<ServiceReleaseRecord>> {
    build_committed_releases_for_services(plan, started, deploy_id, updated_at, None)
}

pub(super) fn build_committed_releases_for_services(
    plan: &ResolvedPlan,
    started: &HashMap<(String, String), InstanceStatusRecord>,
    deploy_id: &DeployId,
    updated_at: u64,
    included_services: Option<&BTreeSet<String>>,
) -> Result<Vec<ServiceReleaseRecord>> {
    let mut releases = Vec::new();
    for service in plan.services() {
        if let Some(included_services) = included_services
            && !included_services.contains(&service.service)
        {
            continue;
        }
        let Some(revision_hash) = service.next_revision_hash() else {
            continue;
        };

        let mut next_slots = Vec::new();
        for slot in &service.slots {
            let active_instance_id = match slot.action {
                DeployChangeKind::Unchanged => {
                    let Some(current) = &slot.current else {
                        return Err(Error::Deploy(DeployError::MissingCurrentSlot {
                            service: service.service.clone(),
                            slot: slot.slot_id.0.clone(),
                        }));
                    };
                    current.active_instance_id.clone()
                }
                DeployChangeKind::Create | DeployChangeKind::Replace => {
                    let key = (service.service.clone(), slot.slot_id.0.clone());
                    let Some(status) = started.get(&key) else {
                        return Err(Error::Deploy(DeployError::MissingStartedInstance {
                            service: service.service.clone(),
                            slot: slot.slot_id.0.clone(),
                        }));
                    };
                    status.instance_id.clone()
                }
                DeployChangeKind::Remove => continue,
            };
            next_slots.push(crate::model::ServiceReleaseSlot {
                slot_id: slot.slot_id.clone(),
                machine_id: slot.machine_id.clone(),
                active_instance_id,
                revision_hash: revision_hash.to_string(),
            });
        }

        releases.push(ServiceReleaseRecord {
            namespace: plan.namespace().clone(),
            service: service.service.clone(),
            release: ServiceRelease {
                primary_revision_hash: revision_hash.to_string(),
                referenced_revision_hashes: vec![revision_hash.to_string()],
                routing: ServiceRoutingPolicy::Direct {
                    revision_hash: revision_hash.to_string(),
                },
                slots: next_slots,
                updated_by_deploy_id: deploy_id.clone(),
                updated_at,
            },
        });
    }
    Ok(releases)
}
