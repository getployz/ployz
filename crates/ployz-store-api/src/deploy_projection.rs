use std::collections::{BTreeMap, HashMap};

use ployz_types::model::{
    DeployId, DeployRecord, RoutingEvent, ServiceReleaseRecord, ServiceRevisionRecord, VolumeRecord,
};
use ployz_types::spec::Namespace;

use crate::DeployCommit;

type RevisionKey = (Namespace, String, String);
type ReleaseKey = (Namespace, String);
type VolumeKey = (Namespace, String);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeployProjection {
    revisions: BTreeMap<RevisionKey, ServiceRevisionRecord>,
    releases: BTreeMap<ReleaseKey, ServiceReleaseRecord>,
    volumes: BTreeMap<VolumeKey, VolumeRecord>,
    deploys: HashMap<DeployId, DeployRecord>,
}

impl DeployProjection {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_commit(&mut self, commit: &DeployCommit) {
        let _events = self.apply_commit_events(commit);
    }

    pub fn apply_commit_events(&mut self, commit: &DeployCommit) -> Vec<RoutingEvent> {
        let mut events = Vec::new();
        for revision in &commit.revisions {
            match self
                .revisions
                .insert(revision_key(revision), revision.clone())
            {
                Some(old) if old != *revision => events.push(RoutingEvent::RevisionUpdated {
                    old,
                    new: revision.clone(),
                }),
                Some(_) => {}
                None => events.push(RoutingEvent::RevisionAdded(revision.clone())),
            }
        }
        for service in &commit.removed_services {
            if let Some(old) = self
                .releases
                .remove(&(commit.namespace.clone(), service.clone()))
            {
                events.push(RoutingEvent::ReleaseRemoved {
                    namespace: old.namespace,
                    service: old.service,
                });
            }
        }
        for volume in &commit.removed_volumes {
            self.volumes
                .remove(&(commit.namespace.clone(), volume.clone()));
        }
        for release in &commit.releases {
            match self.releases.insert(release_key(release), release.clone()) {
                Some(old) if old != *release => events.push(RoutingEvent::ReleaseUpdated {
                    old,
                    new: release.clone(),
                }),
                Some(_) => {}
                None => events.push(RoutingEvent::ReleaseAdded(release.clone())),
            }
        }
        for volume in &commit.volumes {
            self.volumes.insert(volume_key(volume), volume.clone());
        }
        self.deploys
            .insert(commit.deploy.deploy_id.clone(), commit.deploy.clone());
        events
    }

    pub fn update_deploy_record(&mut self, deploy: DeployRecord) {
        self.deploys.insert(deploy.deploy_id.clone(), deploy);
    }

    #[must_use]
    pub fn deploy(&self, deploy_id: &DeployId) -> Option<&DeployRecord> {
        self.deploys.get(deploy_id)
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
    pub fn revisions(&self, namespace: &Namespace) -> Vec<ServiceRevisionRecord> {
        self.revisions
            .values()
            .filter(|revision| &revision.namespace == namespace)
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
    pub fn release(&self, namespace: &Namespace, service: &str) -> Option<&ServiceReleaseRecord> {
        self.releases.get(&(namespace.clone(), service.to_string()))
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

    #[must_use]
    pub fn all_volumes(&self) -> Vec<VolumeRecord> {
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

fn volume_key(record: &VolumeRecord) -> VolumeKey {
    (record.namespace.clone(), record.volume_name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{DeployState, MachineId, ServiceRelease, ServiceRoutingPolicy};
    use ployz_types::spec::VolumeScope;

    #[test]
    fn deploy_projection_returns_namespace_snapshots_in_contract_identity_order() {
        let namespace = Namespace(String::from("prod"));
        let mut projection = DeployProjection::new();
        projection.apply_commit(&commit(
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
            projection
                .revisions(&namespace)
                .iter()
                .map(|record| record.service.as_str())
                .collect::<Vec<_>>(),
            ["api", "worker"]
        );
        assert_eq!(
            projection
                .releases(&namespace)
                .iter()
                .map(|record| record.service.as_str())
                .collect::<Vec<_>>(),
            ["api", "worker"]
        );
        assert_eq!(
            projection
                .volumes(&namespace)
                .iter()
                .map(|record| record.volume_name.as_str())
                .collect::<Vec<_>>(),
            ["a-data", "z-data"]
        );
    }

    #[test]
    fn deploy_projection_returns_global_snapshots_in_contract_identity_order() {
        let prod = Namespace(String::from("prod"));
        let staging = Namespace(String::from("staging"));
        let mut projection = DeployProjection::new();
        projection.apply_commit(&commit(
            &staging,
            vec![revision(&staging, "worker", "rev-b")],
            vec![release(&staging, "worker", "rev-b")],
            vec![volume(&staging, "z-data", "deploy-staging")],
        ));
        projection.apply_commit(&commit(
            &prod,
            vec![revision(&prod, "api", "rev-a")],
            vec![release(&prod, "api", "rev-a")],
            vec![volume(&prod, "a-data", "deploy-prod")],
        ));

        assert_eq!(
            projection
                .all_revisions()
                .iter()
                .map(|record| (record.namespace.0.as_str(), record.service.as_str()))
                .collect::<Vec<_>>(),
            [("prod", "api"), ("staging", "worker")]
        );
        assert_eq!(
            projection
                .all_releases()
                .iter()
                .map(|record| (record.namespace.0.as_str(), record.service.as_str()))
                .collect::<Vec<_>>(),
            [("prod", "api"), ("staging", "worker")]
        );
        assert_eq!(
            projection
                .all_volumes()
                .iter()
                .map(|record| (record.namespace.0.as_str(), record.volume_name.as_str()))
                .collect::<Vec<_>>(),
            [("prod", "a-data"), ("staging", "z-data")]
        );
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
            releases,
            volumes,
            deploy: DeployRecord {
                deploy_id: DeployId(format!("deploy-{}", namespace.0)),
                namespace: namespace.clone(),
                coordinator_machine_id: MachineId("machine-1".into()),
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
            created_by: MachineId("machine-1".into()),
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
                updated_by_deploy_id: DeployId(format!("deploy-{service}")),
                updated_at: 1,
            },
        }
    }

    fn volume(namespace: &Namespace, volume_name: &str, deploy_id: &str) -> VolumeRecord {
        let deploy_id = DeployId(deploy_id.into());
        VolumeRecord {
            namespace: namespace.clone(),
            volume_name: volume_name.into(),
            scope: VolumeScope::Single,
            machine_id: MachineId("machine-1".into()),
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
}
