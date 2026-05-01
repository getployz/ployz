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
                events.push(RoutingEvent::ReleaseRemoved(old));
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
