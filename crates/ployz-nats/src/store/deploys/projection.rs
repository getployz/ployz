use std::collections::HashMap;

use ployz_store_api::DeployCommit;
use ployz_types::model::{
    DeployId, DeployRecord, RoutingEvent, ServiceReleaseRecord, ServiceRevisionRecord, VolumeRecord,
};
use ployz_types::spec::Namespace;

type RevisionKey = (Namespace, String, String);
type ReleaseKey = (Namespace, String);
type VolumeKey = (Namespace, String);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeployProjection {
    revisions: HashMap<RevisionKey, ServiceRevisionRecord>,
    releases: HashMap<ReleaseKey, ServiceReleaseRecord>,
    volumes: HashMap<VolumeKey, VolumeRecord>,
    deploys: HashMap<DeployId, DeployRecord>,
}

impl DeployProjection {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_commit(&mut self, commit: &DeployCommit) {
        for revision in &commit.revisions {
            self.revisions
                .insert(revision_key(revision), revision.clone());
        }
        for service in &commit.removed_services {
            self.releases
                .remove(&(commit.namespace.clone(), service.clone()));
        }
        for volume in &commit.removed_volumes {
            self.volumes
                .remove(&(commit.namespace.clone(), volume.clone()));
        }
        for release in &commit.releases {
            self.releases.insert(release_key(release), release.clone());
        }
        for volume in &commit.volumes {
            self.volumes.insert(volume_key(volume), volume.clone());
        }
        self.deploys
            .insert(commit.deploy.deploy_id.clone(), commit.deploy.clone());
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

    #[must_use]
    pub fn deploy(&self, deploy_id: &DeployId) -> Option<&DeployRecord> {
        self.deploys.get(deploy_id)
    }

    #[must_use]
    pub fn releases(&self, namespace: &Namespace) -> Vec<ServiceReleaseRecord> {
        let mut releases = self
            .releases
            .values()
            .filter(|release| &release.namespace == namespace)
            .cloned()
            .collect::<Vec<_>>();
        sort_releases(&mut releases);
        releases
    }

    #[must_use]
    pub fn revisions(&self, namespace: &Namespace) -> Vec<ServiceRevisionRecord> {
        let mut revisions = self
            .revisions
            .values()
            .filter(|revision| &revision.namespace == namespace)
            .cloned()
            .collect::<Vec<_>>();
        sort_revisions(&mut revisions);
        revisions
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
        let mut volumes = self
            .volumes
            .values()
            .filter(|volume| &volume.namespace == namespace)
            .cloned()
            .collect::<Vec<_>>();
        sort_volumes(&mut volumes);
        volumes
    }

    #[must_use]
    pub fn volume(&self, namespace: &Namespace, volume_name: &str) -> Option<&VolumeRecord> {
        self.volumes
            .get(&(namespace.clone(), volume_name.to_string()))
    }

    #[must_use]
    pub fn all_releases(&self) -> Vec<ServiceReleaseRecord> {
        let mut releases = self.releases.values().cloned().collect::<Vec<_>>();
        sort_releases(&mut releases);
        releases
    }

    #[must_use]
    pub fn all_revisions(&self) -> Vec<ServiceRevisionRecord> {
        let mut revisions = self.revisions.values().cloned().collect::<Vec<_>>();
        sort_revisions(&mut revisions);
        revisions
    }

    #[must_use]
    pub fn all_volumes(&self) -> Vec<VolumeRecord> {
        let mut volumes = self.volumes.values().cloned().collect::<Vec<_>>();
        sort_volumes(&mut volumes);
        volumes
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

fn sort_revisions(revisions: &mut [ServiceRevisionRecord]) {
    revisions.sort_by(|left, right| revision_key(left).cmp(&revision_key(right)));
}

fn sort_releases(releases: &mut [ServiceReleaseRecord]) {
    releases.sort_by(|left, right| release_key(left).cmp(&release_key(right)));
}

fn sort_volumes(volumes: &mut [VolumeRecord]) {
    volumes.sort_by(|left, right| volume_key(left).cmp(&volume_key(right)));
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
            "deploy-1",
            vec![
                revision(&namespace, "worker", "rev-b"),
                revision(&namespace, "api", "rev-b"),
                revision(&namespace, "api", "rev-a"),
            ],
            vec![
                release(&namespace, "worker", "rev-b"),
                release(&namespace, "api", "rev-a"),
            ],
            vec![volume(&namespace, "z-data"), volume(&namespace, "a-data")],
        ));

        assert_eq!(
            projection
                .revisions(&namespace)
                .iter()
                .map(|revision| (revision.service.as_str(), revision.revision_hash.as_str()))
                .collect::<Vec<_>>(),
            [("api", "rev-a"), ("api", "rev-b"), ("worker", "rev-b")]
        );
        assert_eq!(
            projection
                .releases(&namespace)
                .iter()
                .map(|release| release.service.as_str())
                .collect::<Vec<_>>(),
            ["api", "worker"]
        );
        assert_eq!(
            projection
                .volumes(&namespace)
                .iter()
                .map(|volume| volume.volume_name.as_str())
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
            "deploy-staging",
            vec![revision(&staging, "worker", "rev-b")],
            vec![release(&staging, "worker", "rev-b")],
            vec![volume(&staging, "z-data")],
        ));
        projection.apply_commit(&commit(
            &prod,
            "deploy-prod",
            vec![revision(&prod, "api", "rev-a")],
            vec![release(&prod, "api", "rev-a")],
            vec![volume(&prod, "a-data")],
        ));

        assert_eq!(
            projection
                .all_revisions()
                .iter()
                .map(|revision| (
                    revision.namespace.0.as_str(),
                    revision.service.as_str(),
                    revision.revision_hash.as_str()
                ))
                .collect::<Vec<_>>(),
            [("prod", "api", "rev-a"), ("staging", "worker", "rev-b")]
        );
        assert_eq!(
            projection
                .all_releases()
                .iter()
                .map(|release| (release.namespace.0.as_str(), release.service.as_str()))
                .collect::<Vec<_>>(),
            [("prod", "api"), ("staging", "worker")]
        );
        assert_eq!(
            projection
                .all_volumes()
                .iter()
                .map(|volume| (volume.namespace.0.as_str(), volume.volume_name.as_str()))
                .collect::<Vec<_>>(),
            [("prod", "a-data"), ("staging", "z-data")]
        );
    }

    fn revision(
        namespace: &Namespace,
        service: &str,
        revision_hash: &str,
    ) -> ServiceRevisionRecord {
        ServiceRevisionRecord {
            namespace: namespace.clone(),
            service: service.into(),
            revision_hash: revision_hash.into(),
            spec_json: "{}".into(),
            created_by: MachineId(String::from("founder")),
            created_at: 1,
        }
    }

    fn release(namespace: &Namespace, service: &str, revision_hash: &str) -> ServiceReleaseRecord {
        ServiceReleaseRecord {
            namespace: namespace.clone(),
            service: service.into(),
            release: ServiceRelease {
                primary_revision_hash: revision_hash.into(),
                referenced_revision_hashes: vec![revision_hash.into()],
                routing: ServiceRoutingPolicy::Direct {
                    revision_hash: revision_hash.into(),
                },
                slots: Vec::new(),
                updated_by_deploy_id: DeployId(String::from("deploy-1")),
                updated_at: 1,
            },
        }
    }

    fn volume(namespace: &Namespace, name: &str) -> VolumeRecord {
        VolumeRecord {
            namespace: namespace.clone(),
            volume_name: name.into(),
            scope: VolumeScope::Single,
            machine_id: MachineId(String::from("machine-a")),
            quota: String::from("1G"),
            mode: String::from("0755"),
            owner: String::from("1000:1000"),
            attached_services: Vec::new(),
            created_at: 1,
            created_by_deploy_id: DeployId(String::from("deploy-1")),
            last_modified_at: 1,
            last_modified_by_deploy_id: DeployId(String::from("deploy-1")),
        }
    }

    fn commit(
        namespace: &Namespace,
        deploy_id: &str,
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
                deploy_id: DeployId(deploy_id.into()),
                namespace: namespace.clone(),
                coordinator_machine_id: MachineId(String::from("founder")),
                manifest_hash: String::from("manifest"),
                state: DeployState::Committed,
                started_at: 1,
                committed_at: Some(2),
                finished_at: Some(2),
                summary_json: String::from("{}"),
            },
        }
    }
}
