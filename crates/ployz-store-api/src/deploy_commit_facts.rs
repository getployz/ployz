use std::collections::BTreeMap;

use ployz_types::model::{
    DeployId, DeployPhaseCommitRecord, DeployPhaseId, RoutingEvent, ServiceBranchLineageRecord,
    ServiceReleaseRecord, ServiceRevisionRecord, VolumeBranchLineageRecord, VolumeMovementRecord,
    VolumeRecord,
};
use ployz_types::spec::Namespace;

use crate::DeployCommit;

type RevisionKey = (Namespace, String, String);
type ReleaseKey = (Namespace, String);
type BranchLineageKey = (Namespace, String, String);
type VolumeMovementKey = (Namespace, String, String);
type VolumeBranchKey = (Namespace, String, String);
type PhaseCommitKey = (Namespace, String, DeployPhaseId);
type VolumeKey = (Namespace, String);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeployCommitFacts {
    revisions: BTreeMap<RevisionKey, ServiceRevisionRecord>,
    releases: BTreeMap<ReleaseKey, ServiceReleaseRecord>,
    branch_lineage: BTreeMap<BranchLineageKey, ServiceBranchLineageRecord>,
    volume_movements: BTreeMap<VolumeMovementKey, VolumeMovementRecord>,
    volume_branches: BTreeMap<VolumeBranchKey, VolumeBranchLineageRecord>,
    phase_commits: BTreeMap<PhaseCommitKey, DeployPhaseCommitRecord>,
    volumes: BTreeMap<VolumeKey, VolumeRecord>,
}

impl DeployCommitFacts {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_commit_events(&mut self, commit: &DeployCommit) -> Vec<RoutingEvent> {
        let mut events = Vec::new();
        for revision in &commit.revisions {
            self.revisions
                .insert(revision_key(revision), revision.clone());
            events.push(RoutingEvent::RevisionUpsert(revision.clone()));
        }
        for service in &commit.removed_services {
            self.releases
                .remove(&(commit.namespace.clone(), service.clone()));
            let removed_lineage = self
                .branch_lineage
                .keys()
                .filter(|(namespace, lineage_service, _revision_hash)| {
                    namespace == &commit.namespace && lineage_service == service
                })
                .cloned()
                .collect::<Vec<_>>();
            for key in removed_lineage {
                self.branch_lineage.remove(&key);
            }
            events.push(RoutingEvent::ReleaseRemoved {
                namespace: commit.namespace.clone(),
                service: service.clone(),
            });
        }
        for volume in &commit.removed_volumes {
            self.volumes
                .remove(&(commit.namespace.clone(), volume.clone()));
            let removed_movements = self
                .volume_movements
                .keys()
                .filter(|(namespace, movement_volume, _deploy_id)| {
                    namespace == &commit.namespace && movement_volume == volume
                })
                .cloned()
                .collect::<Vec<_>>();
            for key in removed_movements {
                self.volume_movements.remove(&key);
            }
            let removed_branches = self
                .volume_branches
                .keys()
                .filter(|(namespace, branch_volume, _deploy_id)| {
                    namespace == &commit.namespace && branch_volume == volume
                })
                .cloned()
                .collect::<Vec<_>>();
            for key in removed_branches {
                self.volume_branches.remove(&key);
            }
        }
        for release in &commit.releases {
            self.releases.insert(release_key(release), release.clone());
            events.push(RoutingEvent::ReleaseUpsert(release.clone()));
        }
        for lineage in &commit.branch_lineage {
            self.branch_lineage
                .insert(branch_lineage_key(lineage), lineage.clone());
        }
        for movement in valid_volume_movements(commit) {
            self.volume_movements
                .insert(volume_movement_key(movement), movement.clone());
        }
        for branch in valid_volume_branches(commit) {
            self.volume_branches
                .insert(volume_branch_key(branch), branch.clone());
        }
        for phase_commit in valid_phase_commits(commit) {
            self.phase_commits
                .insert(phase_commit_key(phase_commit), phase_commit.clone());
        }
        for volume in &commit.volumes {
            self.volumes.insert(volume_key(volume), volume.clone());
        }
        events
    }

    #[must_use]
    pub fn release(&self, namespace: &Namespace, service: &str) -> Option<&ServiceReleaseRecord> {
        self.releases.get(&(namespace.clone(), service.to_string()))
    }

    #[must_use]
    pub fn releases(&self, namespace: &Namespace) -> Vec<ServiceReleaseRecord> {
        self.releases
            .values()
            .filter(|release| &release.namespace == namespace)
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn revision(
        &self,
        namespace: &Namespace,
        service: &str,
        revision_hash: &str,
    ) -> Option<&ServiceRevisionRecord> {
        self.revisions.get(&(
            namespace.clone(),
            service.to_string(),
            revision_hash.to_string(),
        ))
    }

    #[must_use]
    pub fn revisions(&self, namespace: &Namespace) -> Vec<ServiceRevisionRecord> {
        self.revisions
            .values()
            .filter(|revision| &revision.namespace == namespace)
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn volumes(&self, namespace: &Namespace) -> Vec<VolumeRecord> {
        self.volumes
            .values()
            .filter(|volume| &volume.namespace == namespace)
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn service_branch_lineage(&self, namespace: &Namespace) -> Vec<ServiceBranchLineageRecord> {
        self.branch_lineage
            .values()
            .filter(|lineage| &lineage.namespace == namespace)
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn volume_movements(&self, namespace: &Namespace) -> Vec<VolumeMovementRecord> {
        self.volume_movements
            .values()
            .filter(|movement| &movement.namespace == namespace)
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn volume_branches(&self, namespace: &Namespace) -> Vec<VolumeBranchLineageRecord> {
        self.volume_branches
            .values()
            .filter(|branch| &branch.namespace == namespace)
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn phase_commits(
        &self,
        namespace: &Namespace,
        deploy_id: &ployz_types::model::DeployId,
    ) -> Vec<DeployPhaseCommitRecord> {
        self.phase_commits
            .values()
            .filter(|record| &record.namespace == namespace && &record.deploy_id == deploy_id)
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn phase_commit(
        &self,
        namespace: &Namespace,
        deploy_id: &ployz_types::model::DeployId,
        phase_id: &DeployPhaseId,
    ) -> Option<&DeployPhaseCommitRecord> {
        self.phase_commits.get(&(
            namespace.clone(),
            deploy_id.as_str().to_string(),
            phase_id.clone(),
        ))
    }

    #[must_use]
    pub fn volume(&self, namespace: &Namespace, volume_name: &str) -> Option<&VolumeRecord> {
        self.volumes
            .get(&(namespace.clone(), volume_name.to_string()))
    }

    #[must_use]
    pub fn all_releases(&self) -> Vec<ServiceReleaseRecord> {
        self.releases.values().cloned().collect()
    }

    #[must_use]
    pub fn all_revisions(&self) -> Vec<ServiceRevisionRecord> {
        self.revisions.values().cloned().collect()
    }

    #[cfg(test)]
    #[must_use]
    fn all_volumes(&self) -> Vec<VolumeRecord> {
        self.volumes.values().cloned().collect()
    }
}

fn revision_key(record: &ServiceRevisionRecord) -> RevisionKey {
    (
        record.namespace.clone(),
        record.service.clone(),
        record.revision_hash.clone(),
    )
}

fn release_key(record: &ServiceReleaseRecord) -> ReleaseKey {
    (record.namespace.clone(), record.service.clone())
}

fn branch_lineage_key(record: &ServiceBranchLineageRecord) -> BranchLineageKey {
    (
        record.namespace.clone(),
        record.service.clone(),
        record.revision_hash.clone(),
    )
}

fn volume_movement_key(record: &VolumeMovementRecord) -> VolumeMovementKey {
    (
        record.namespace.clone(),
        record.volume_name.clone(),
        record.deploy_id.as_str().to_string(),
    )
}

fn volume_branch_key(record: &VolumeBranchLineageRecord) -> VolumeBranchKey {
    (
        record.namespace.clone(),
        record.volume_name.clone(),
        record.deploy_id.as_str().to_string(),
    )
}

fn phase_commit_key(record: &DeployPhaseCommitRecord) -> PhaseCommitKey {
    (
        record.namespace.clone(),
        record.deploy_id.as_str().to_string(),
        record.phase_id.clone(),
    )
}

fn volume_key(record: &VolumeRecord) -> VolumeKey {
    (record.namespace.clone(), record.volume_name.clone())
}

fn valid_volume_movements(commit: &DeployCommit) -> impl Iterator<Item = &VolumeMovementRecord> {
    commit.volume_movements.iter().filter(|movement| {
        movement.namespace == commit.namespace
            && movement.commit_deploy_id == commit.deploy.deploy_id
            && evidence_deploy_id_matches_commit(
                &movement.deploy_id,
                movement.phase_id.as_ref(),
                &commit.deploy.deploy_id,
            )
            && commit.volumes.iter().any(|volume| {
                volume.namespace == movement.namespace
                    && volume.volume_name == movement.volume_name
                    && volume.machine_id == movement.final_machine
            })
    })
}

fn valid_volume_branches(
    commit: &DeployCommit,
) -> impl Iterator<Item = &VolumeBranchLineageRecord> {
    commit.volume_branches.iter().filter(|branch| {
        branch.namespace == commit.namespace
            && branch.commit_deploy_id == commit.deploy.deploy_id
            && commit.volumes.iter().any(|volume| {
                volume.namespace == branch.namespace
                    && volume.volume_name == branch.volume_name
                    && volume.machine_id == branch.target_machine
            })
    })
}

fn valid_phase_commits(commit: &DeployCommit) -> impl Iterator<Item = &DeployPhaseCommitRecord> {
    commit.phase_commits.iter().filter(|record| {
        record.namespace == commit.namespace
            && record.commit_deploy_id == commit.deploy.deploy_id
            && evidence_deploy_id_matches_commit(
                &record.deploy_id,
                Some(&record.phase_id),
                &commit.deploy.deploy_id,
            )
    })
}

fn evidence_deploy_id_matches_commit(
    deploy_id: &DeployId,
    phase_id: Option<&DeployPhaseId>,
    commit_deploy_id: &DeployId,
) -> bool {
    if deploy_id == commit_deploy_id {
        return true;
    }
    match phase_id {
        Some(phase_id) => phase_commit_deploy_id(deploy_id, phase_id) == *commit_deploy_id,
        None => false,
    }
}

fn phase_commit_deploy_id(deploy_id: &DeployId, phase_id: &DeployPhaseId) -> DeployId {
    DeployId::new(format!(
        "{}:phase:{}",
        deploy_id.as_str(),
        phase_id.as_str()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{
        DeployId, DeployPhaseCommitRecord, DeployPhaseId, DeployRecord, DeployState, MachineId,
        ServiceBranchLineageRecord, ServiceRelease, ServiceRoutingPolicy, VolumeMovementRecord,
    };
    use ployz_types::spec::VolumeScope;

    #[test]
    fn deploy_commit_facts_returns_namespace_snapshots_in_contract_identity_order() {
        let namespace = Namespace::new(String::from("prod"));
        let mut facts = DeployCommitFacts::new();
        facts.apply_commit_events(&commit(
            &namespace,
            vec![
                revision(&namespace, "worker", "rev-b"),
                revision(&namespace, "api", "rev-a"),
            ],
            vec![
                release(&namespace, "worker", "rev-b"),
                release(&namespace, "api", "rev-a"),
            ],
            vec![
                volume(&namespace, "z-data", "deploy-1"),
                volume(&namespace, "a-data", "deploy-1"),
            ],
        ));

        assert_eq!(
            facts
                .revisions(&namespace)
                .iter()
                .map(|record| record.service.as_str())
                .collect::<Vec<_>>(),
            ["api", "worker"]
        );
        assert_eq!(
            facts
                .releases(&namespace)
                .iter()
                .map(|record| record.service.as_str())
                .collect::<Vec<_>>(),
            ["api", "worker"]
        );
        assert_eq!(
            facts
                .volumes(&namespace)
                .iter()
                .map(|record| record.volume_name.as_str())
                .collect::<Vec<_>>(),
            ["a-data", "z-data"]
        );
    }

    #[test]
    fn deploy_commit_facts_returns_global_snapshots_in_contract_identity_order() {
        let prod = Namespace::new(String::from("prod"));
        let staging = Namespace::new(String::from("staging"));
        let mut facts = DeployCommitFacts::new();
        facts.apply_commit_events(&commit(
            &staging,
            vec![revision(&staging, "worker", "rev-b")],
            vec![release(&staging, "worker", "rev-b")],
            vec![volume(&staging, "z-data", "deploy-staging")],
        ));
        facts.apply_commit_events(&commit(
            &prod,
            vec![revision(&prod, "api", "rev-a")],
            vec![release(&prod, "api", "rev-a")],
            vec![volume(&prod, "a-data", "deploy-prod")],
        ));

        assert_eq!(
            facts
                .all_revisions()
                .iter()
                .map(|record| (record.namespace.as_str(), record.service.as_str()))
                .collect::<Vec<_>>(),
            [("prod", "api"), ("staging", "worker")]
        );
        assert_eq!(
            facts
                .all_releases()
                .iter()
                .map(|record| (record.namespace.as_str(), record.service.as_str()))
                .collect::<Vec<_>>(),
            [("prod", "api"), ("staging", "worker")]
        );
        assert_eq!(
            facts
                .all_volumes()
                .iter()
                .map(|record| (record.namespace.as_str(), record.volume_name.as_str()))
                .collect::<Vec<_>>(),
            [("prod", "a-data"), ("staging", "z-data")]
        );
    }

    #[test]
    fn deploy_commit_events_are_commit_facts_not_prior_state_diffs() {
        let namespace = Namespace::new(String::from("prod"));
        let mut facts = DeployCommitFacts::new();
        let release = release(&namespace, "api", "rev-a");
        facts.apply_commit_events(&commit(
            &namespace,
            vec![revision(&namespace, "api", "rev-a")],
            vec![release.clone()],
            Vec::new(),
        ));
        let mut repeated = commit(
            &namespace,
            vec![revision(&namespace, "api", "rev-a")],
            vec![release.clone()],
            Vec::new(),
        );
        repeated.removed_services = vec![String::from("missing")];

        let events = facts.apply_commit_events(&repeated);

        assert_eq!(
            events,
            vec![
                RoutingEvent::RevisionUpsert(revision(&namespace, "api", "rev-a")),
                RoutingEvent::ReleaseRemoved {
                    namespace: namespace.clone(),
                    service: String::from("missing"),
                },
                RoutingEvent::ReleaseUpsert(release),
            ]
        );
    }

    #[test]
    fn deploy_commit_facts_records_branch_lineage_with_commit_facts() {
        let namespace = Namespace::new(String::from("pr-39"));
        let mut facts = DeployCommitFacts::new();
        let mut command = commit(
            &namespace,
            vec![revision(&namespace, "web", "rev-target")],
            vec![release(&namespace, "web", "rev-target")],
            Vec::new(),
        );
        command.branch_lineage = vec![branch_lineage(&namespace, "web", "rev-target")];

        facts.apply_commit_events(&command);

        assert_eq!(
            facts.service_branch_lineage(&namespace),
            vec![branch_lineage(&namespace, "web", "rev-target")]
        );
    }

    #[test]
    fn deploy_commit_facts_records_volume_movements_with_commit_facts() {
        let namespace = Namespace::new(String::from("prod"));
        let mut facts = DeployCommitFacts::new();
        let mut command = commit(
            &namespace,
            Vec::new(),
            Vec::new(),
            vec![moved_volume(&namespace, "pgdata", "deploy-move")],
        );
        command.volume_movements = vec![volume_movement(&namespace, "pgdata", "deploy-prod")];

        facts.apply_commit_events(&command);

        assert_eq!(
            facts.volume_movements(&namespace),
            vec![volume_movement(&namespace, "pgdata", "deploy-prod")]
        );
    }

    #[test]
    fn deploy_commit_facts_records_checkpoint_volume_movement_linkage() {
        let namespace = Namespace::new(String::from("prod"));
        let mut facts = DeployCommitFacts::new();
        let mut command = commit(
            &namespace,
            Vec::new(),
            Vec::new(),
            vec![moved_volume(&namespace, "pgdata", "deploy-prod:phase:db")],
        );
        command.deploy.deploy_id = DeployId::new("deploy-prod:phase:db");
        let mut movement = volume_movement(&namespace, "pgdata", "deploy-prod");
        movement.commit_deploy_id = DeployId::new("deploy-prod:phase:db");
        movement.phase_id = Some(DeployPhaseId::new("db"));
        command.volume_movements = vec![movement.clone()];

        facts.apply_commit_events(&command);

        assert_eq!(facts.volume_movements(&namespace), vec![movement]);
    }

    #[test]
    fn deploy_commit_facts_ignores_volume_movement_for_unrelated_deploy() {
        let namespace = Namespace::new(String::from("prod"));
        let mut facts = DeployCommitFacts::new();
        let mut command = commit(
            &namespace,
            Vec::new(),
            Vec::new(),
            vec![moved_volume(&namespace, "pgdata", "deploy-prod:phase:db")],
        );
        command.deploy.deploy_id = DeployId::new("deploy-prod:phase:db");
        let mut movement = volume_movement(&namespace, "pgdata", "other-deploy");
        movement.commit_deploy_id = DeployId::new("deploy-prod:phase:db");
        movement.phase_id = Some(DeployPhaseId::new("db"));
        command.volume_movements = vec![movement];

        facts.apply_commit_events(&command);

        assert!(facts.volume_movements(&namespace).is_empty());
    }

    #[test]
    fn deploy_commit_facts_ignores_orphaned_volume_movement_evidence() {
        let namespace = Namespace::new(String::from("prod"));
        let mut facts = DeployCommitFacts::new();
        let mut command = commit(&namespace, Vec::new(), Vec::new(), Vec::new());
        command.volume_movements = vec![volume_movement(&namespace, "pgdata", "deploy-prod")];

        facts.apply_commit_events(&command);

        assert!(facts.volume_movements(&namespace).is_empty());
    }

    #[test]
    fn deploy_commit_facts_ignores_mismatched_volume_movement_evidence() {
        let namespace = Namespace::new(String::from("prod"));
        let mut facts = DeployCommitFacts::new();
        let mut command = commit(
            &namespace,
            Vec::new(),
            Vec::new(),
            vec![moved_volume(&namespace, "pgdata", "deploy-prod")],
        );
        let mut movement = volume_movement(&namespace, "pgdata", "deploy-prod");
        movement.final_machine = MachineId::new("machine-c");
        command.volume_movements = vec![movement];

        facts.apply_commit_events(&command);

        assert!(facts.volume_movements(&namespace).is_empty());
    }

    #[test]
    fn deploy_commit_facts_records_phase_commit_linkage_with_commit_facts() {
        let namespace = Namespace::new(String::from("prod"));
        let mut facts = DeployCommitFacts::new();
        let mut command = commit(&namespace, Vec::new(), Vec::new(), Vec::new());
        command.phase_commits = vec![phase_commit(&namespace, "deploy-prod", "deploy")];

        facts.apply_commit_events(&command);

        assert_eq!(
            facts.phase_commits(&namespace, &DeployId::new("deploy-prod")),
            vec![phase_commit(&namespace, "deploy-prod", "deploy")]
        );
    }

    #[test]
    fn deploy_commit_facts_records_checkpoint_phase_commit_linkage() {
        let namespace = Namespace::new(String::from("prod"));
        let mut facts = DeployCommitFacts::new();
        let mut command = commit(&namespace, Vec::new(), Vec::new(), Vec::new());
        command.deploy.deploy_id = DeployId::new("deploy-prod:phase:db");
        command.phase_commits = vec![phase_commit_with_commit_id(
            &namespace,
            "deploy-prod",
            "db",
            "deploy-prod:phase:db",
        )];

        facts.apply_commit_events(&command);

        assert_eq!(
            facts.phase_commits(&namespace, &DeployId::new("deploy-prod")),
            vec![phase_commit_with_commit_id(
                &namespace,
                "deploy-prod",
                "db",
                "deploy-prod:phase:db",
            )]
        );
    }

    #[test]
    fn deploy_commit_facts_ignores_phase_commit_for_unrelated_deploy() {
        let namespace = Namespace::new(String::from("prod"));
        let mut facts = DeployCommitFacts::new();
        let mut command = commit(&namespace, Vec::new(), Vec::new(), Vec::new());
        command.deploy.deploy_id = DeployId::new("deploy-prod:phase:db");
        command.phase_commits = vec![phase_commit_with_commit_id(
            &namespace,
            "other-deploy",
            "db",
            "deploy-prod:phase:db",
        )];

        facts.apply_commit_events(&command);

        assert!(
            facts
                .phase_commits(&namespace, &DeployId::new("other-deploy"))
                .is_empty()
        );
    }

    #[test]
    fn deploy_commit_facts_removes_volume_movements_when_volume_is_removed() {
        let namespace = Namespace::new(String::from("prod"));
        let mut facts = DeployCommitFacts::new();
        let mut command = commit(
            &namespace,
            Vec::new(),
            Vec::new(),
            vec![moved_volume(&namespace, "pgdata", "deploy-move")],
        );
        command.volume_movements = vec![volume_movement(&namespace, "pgdata", "deploy-prod")];
        facts.apply_commit_events(&command);

        let mut remove = commit(&namespace, Vec::new(), Vec::new(), Vec::new());
        remove.removed_volumes = vec![String::from("pgdata")];
        facts.apply_commit_events(&remove);

        assert!(facts.volume_movements(&namespace).is_empty());
    }

    #[test]
    fn deploy_commit_facts_removes_branch_lineage_when_service_is_removed() {
        let namespace = Namespace::new(String::from("pr-39"));
        let mut facts = DeployCommitFacts::new();
        let mut command = commit(
            &namespace,
            vec![revision(&namespace, "web", "rev-target")],
            vec![release(&namespace, "web", "rev-target")],
            Vec::new(),
        );
        command.branch_lineage = vec![branch_lineage(&namespace, "web", "rev-target")];
        facts.apply_commit_events(&command);

        let mut remove = commit(&namespace, Vec::new(), Vec::new(), Vec::new());
        remove.removed_services = vec![String::from("web")];
        facts.apply_commit_events(&remove);

        assert!(facts.service_branch_lineage(&namespace).is_empty());
    }

    fn commit(
        namespace: &Namespace,
        revisions: Vec<ServiceRevisionRecord>,
        releases: Vec<ServiceReleaseRecord>,
        volumes: Vec<VolumeRecord>,
    ) -> DeployCommit {
        DeployCommit {
            namespace: namespace.clone(),
            revisions,
            removed_services: Vec::new(),
            removed_volumes: Vec::new(),
            branch_lineage: Vec::new(),
            volume_movements: Vec::new(),
            volume_branches: Vec::new(),
            phase_commits: Vec::new(),
            releases,
            volumes,
            deploy: DeployRecord {
                deploy_id: DeployId::new(format!("deploy-{}", namespace.as_str())),
                namespace: namespace.clone(),
                coordinator_machine_id: MachineId::new("machine-1"),
                manifest_hash: "manifest".into(),
                state: DeployState::Committed,
                started_at: 1,
                committed_at: Some(2),
                finished_at: Some(2),
                summary_json: "{}".into(),
            },
        }
    }

    fn revision(
        namespace: &Namespace,
        service: &str,
        revision_hash: &str,
    ) -> ServiceRevisionRecord {
        ServiceRevisionRecord {
            namespace: namespace.clone(),
            service: service.to_string(),
            revision_hash: revision_hash.to_string(),
            spec_json: "{}".to_string(),
            created_by: MachineId::new("machine-1"),
            created_at: 1,
        }
    }

    fn release(namespace: &Namespace, service: &str, revision_hash: &str) -> ServiceReleaseRecord {
        ServiceReleaseRecord {
            namespace: namespace.clone(),
            service: service.to_string(),
            release: ServiceRelease {
                primary_revision_hash: revision_hash.to_string(),
                referenced_revision_hashes: vec![revision_hash.to_string()],
                routing: ServiceRoutingPolicy::Direct {
                    revision_hash: revision_hash.to_string(),
                },
                slots: Vec::new(),
                updated_by_deploy_id: DeployId::new(format!("deploy-{service}")),
                updated_at: 1,
            },
        }
    }

    fn volume(namespace: &Namespace, volume_name: &str, deploy_id: &str) -> VolumeRecord {
        let deploy_id = DeployId::new(deploy_id);
        VolumeRecord {
            namespace: namespace.clone(),
            volume_name: volume_name.into(),
            scope: VolumeScope::Single,
            machine_id: MachineId::new("machine-1"),
            quota: "1G".into(),
            mode: "0750".into(),
            owner: "999:999".into(),
            attached_services: Vec::new(),
            created_at: 1,
            created_by_deploy_id: deploy_id.clone(),
            last_modified_at: 1,
            last_modified_by_deploy_id: deploy_id,
        }
    }

    fn moved_volume(namespace: &Namespace, volume_name: &str, deploy_id: &str) -> VolumeRecord {
        let mut volume = volume(namespace, volume_name, deploy_id);
        volume.machine_id = MachineId::new("machine-b");
        volume
    }

    fn branch_lineage(
        namespace: &Namespace,
        service: &str,
        revision_hash: &str,
    ) -> ServiceBranchLineageRecord {
        ServiceBranchLineageRecord {
            namespace: namespace.clone(),
            service: service.into(),
            revision_hash: revision_hash.into(),
            source_namespace: Namespace::new("prod"),
            source_service: service.into(),
            source_revision_hash: "rev-source".into(),
            deploy_id: DeployId::new("deploy-branch"),
            created_at: 2,
        }
    }

    fn volume_movement(
        namespace: &Namespace,
        volume_name: &str,
        deploy_id: &str,
    ) -> VolumeMovementRecord {
        let deploy_id = DeployId::new(deploy_id);
        VolumeMovementRecord {
            namespace: namespace.clone(),
            volume_name: volume_name.into(),
            from_machine: MachineId::new("machine-a"),
            to_machine: MachineId::new("machine-b"),
            final_machine: MachineId::new("machine-b"),
            deploy_id: deploy_id.clone(),
            commit_deploy_id: deploy_id,
            phase_id: None,
            snapshot_name: "ployz-deploy-snap".into(),
            snapshot_guid: 42,
            bytes_transferred: 4096,
            created_at: 2,
        }
    }

    fn phase_commit(
        namespace: &Namespace,
        deploy_id: &str,
        phase_id: &str,
    ) -> DeployPhaseCommitRecord {
        phase_commit_with_commit_id(namespace, deploy_id, phase_id, deploy_id)
    }

    fn phase_commit_with_commit_id(
        namespace: &Namespace,
        deploy_id: &str,
        phase_id: &str,
        commit_deploy_id: &str,
    ) -> DeployPhaseCommitRecord {
        let deploy_id = DeployId::new(deploy_id);
        DeployPhaseCommitRecord {
            namespace: namespace.clone(),
            deploy_id: deploy_id.clone(),
            phase_id: DeployPhaseId::new(phase_id),
            commit_deploy_id: DeployId::new(commit_deploy_id),
            committed_at: 2,
        }
    }
}
