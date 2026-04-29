use crate::deploy::plan::ResolvedPlan;
use crate::error::{Error, Result};
use crate::model::{
    DeployChangeKind, DeployEvent, DeployId, DeployPreview, DeployRecord, DeployState,
    InstanceStatusRecord, MachineId, ServiceRelease, ServiceReleaseRecord, ServiceRevisionRecord,
    ServiceRoutingPolicy, VolumeRecord,
};
use ployz_store_api::DeployCommit;
use ployz_types::spec::Namespace;
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

#[derive(Debug)]
pub(super) struct StartedCandidates {
    prepared: PreparedDeploy,
    started: HashMap<(String, String), InstanceStatusRecord>,
}

#[derive(Debug)]
pub(super) struct CommitPlan {
    deploy_id: DeployId,
    plan: ResolvedPlan,
    preview: DeployPreview,
    participants: BTreeSet<MachineId>,
    commit: DeployCommit,
}

#[derive(Debug)]
pub(super) struct CommittedDeploy {
    deploy_id: DeployId,
    namespace: Namespace,
    plan: ResolvedPlan,
    preview: DeployPreview,
    participants: BTreeSet<MachineId>,
    releases: Vec<ServiceReleaseRecord>,
    deploy: DeployRecord,
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

    pub(super) fn applying_record(&self) -> &DeployRecord {
        &self.applying_record
    }

    pub(super) fn revisions(&self) -> &[ServiceRevisionRecord] {
        &self.revisions
    }

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

impl StartedCandidates {
    pub(super) fn started(&self) -> &HashMap<(String, String), InstanceStatusRecord> {
        &self.started
    }

    pub(super) fn plan(&self) -> &ResolvedPlan {
        &self.prepared.plan
    }

    pub(super) fn deploy_id(&self) -> &DeployId {
        &self.prepared.deploy_id
    }

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
            revisions: _,
        } = self.prepared;
        let committed_at = now_unix_secs();
        let releases = build_committed_releases(&plan, &self.started, &deploy_id, committed_at)?;
        let removed_services = plan
            .services()
            .iter()
            .filter(|service| service.action == DeployChangeKind::Remove)
            .map(|service| service.service.clone())
            .collect::<Vec<_>>();
        let deploy = DeployRecord {
            state: DeployState::Committed,
            committed_at: Some(committed_at),
            finished_at: Some(committed_at),
            summary_json: serde_json::to_string(&preview).map_err(|error| {
                Error::operation("deploy_apply", format!("serialize preview: {error}"))
            })?,
            ..applying_record
        };
        let namespace = plan.namespace().clone();
        let participants = plan.participants().clone();

        Ok(CommitPlan {
            deploy_id,
            plan,
            preview,
            participants,
            commit: DeployCommit {
                namespace,
                removed_services,
                removed_volumes,
                releases,
                volumes,
                deploy,
            },
        })
    }
}

impl CommitPlan {
    pub(super) fn commit(&self) -> &DeployCommit {
        &self.commit
    }

    pub(super) fn into_committed(self) -> CommittedDeploy {
        let Self {
            deploy_id,
            plan,
            preview,
            participants,
            commit,
        } = self;
        CommittedDeploy {
            deploy_id,
            namespace: commit.namespace,
            plan,
            preview,
            participants,
            releases: commit.releases,
            deploy: commit.deploy,
        }
    }
}

impl CommittedDeploy {
    pub(super) fn commit_event(&self) -> DeployEvent {
        DeployEvent {
            step: "commit".into(),
            message: format!(
                "committed deploy {} for '{}'",
                self.deploy_id, self.namespace
            ),
        }
    }

    pub(super) fn cleanup_plan(&self) -> CleanupPlan {
        let active_instance_ids = self
            .releases
            .iter()
            .flat_map(|release| release.release.slots.iter())
            .map(|slot| slot.active_instance_id.0.clone())
            .collect::<BTreeSet<_>>();
        CleanupPlan {
            namespace: self.namespace.clone(),
            participants: self.participants.clone(),
            active_instance_ids,
        }
    }

    pub(super) fn cleanup_pending_record(&self, finished_at: u64) -> DeployRecord {
        DeployRecord {
            state: DeployState::CleanupPending,
            finished_at: Some(finished_at),
            ..self.deploy.clone()
        }
    }

    pub(super) fn deploy_id(&self) -> &DeployId {
        &self.deploy_id
    }

    pub(super) fn preview(&self) -> &DeployPreview {
        &self.preview
    }

    pub(super) fn plan(&self) -> &ResolvedPlan {
        &self.plan
    }

    pub(super) fn deploy_record(&self) -> &DeployRecord {
        &self.deploy
    }

    pub(super) fn set_warnings(&mut self, warnings: Vec<String>) -> Result<()> {
        self.preview.warnings = warnings;
        self.deploy.summary_json = serde_json::to_string(&self.preview).map_err(|error| {
            Error::operation("deploy_apply", format!("serialize preview: {error}"))
        })?;
        Ok(())
    }
}

impl CleanupPlan {
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

fn build_committed_releases(
    plan: &ResolvedPlan,
    started: &HashMap<(String, String), InstanceStatusRecord>,
    deploy_id: &DeployId,
    updated_at: u64,
) -> Result<Vec<ServiceReleaseRecord>> {
    let mut releases = Vec::new();
    for service in plan.services() {
        let Some(revision_hash) = service.next_revision_hash() else {
            continue;
        };

        let mut next_slots = Vec::new();
        for slot in &service.slots {
            let active_instance_id = match slot.action {
                DeployChangeKind::Unchanged => {
                    let Some(current) = &slot.current else {
                        return Err(Error::operation(
                            "deploy_apply",
                            format!(
                                "missing current slot for unchanged service '{}' slot '{}'",
                                service.service, slot.slot_id
                            ),
                        ));
                    };
                    current.active_instance_id.clone()
                }
                DeployChangeKind::Create | DeployChangeKind::Replace => {
                    let key = (service.service.clone(), slot.slot_id.0.clone());
                    let Some(status) = started.get(&key) else {
                        return Err(Error::operation(
                            "deploy_apply",
                            format!(
                                "missing started instance for service '{}' slot '{}'",
                                service.service, slot.slot_id
                            ),
                        ));
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
