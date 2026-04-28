use crate::{
    AcmeChallengeSubscription, CertificateStore, CertificateSubscription, DeployCommit,
    DeployRecordUpdate, DeployRepository, DeployRevisionUpsert, DeploySnapshot,
    InstanceStatusRepository, InviteRepository, MachineRegistry, RoutingInvalidationSubscription,
    RoutingSnapshotReader, StoreRuntimeControl, SyncProbe, SyncStatus,
};
use async_trait::async_trait;
use ployz_types::error::{Error, Result};
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeEvent, AcmeChallengeRecord, CertificateEvent,
    CertificateRecord, DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord,
    MachineEvent, MachineId, MachineMembership, RoutingState, ServiceReleaseRecord,
    ServiceRevisionRecord, VolumeRecord,
};
use ployz_types::spec::Namespace;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use tokio::sync::mpsc;
use tracing::warn;

pub struct MemoryStore {
    inner: Mutex<StoreInner>,
}

struct StoreInner {
    machines: HashMap<MachineId, MachineMembership>,
    machine_subscribers: Vec<mpsc::Sender<MachineEvent>>,
    routing_subscribers: Vec<mpsc::Sender<()>>,
    invites: HashMap<String, InviteRecord>,
    service_revisions: HashMap<(Namespace, String, String), ServiceRevisionRecord>,
    service_releases: HashMap<(Namespace, String), ServiceReleaseRecord>,
    volumes: HashMap<(Namespace, String), VolumeRecord>,
    instance_status: HashMap<InstanceId, InstanceStatusRecord>,
    deploys: HashMap<DeployId, DeployRecord>,
    acme_accounts: HashMap<String, AcmeAccountRecord>,
    certificates: HashMap<String, CertificateRecord>,
    certificate_subscribers: Vec<mpsc::Sender<CertificateEvent>>,
    acme_challenges: HashMap<(String, String), AcmeChallengeRecord>,
    acme_challenge_subscribers: Vec<mpsc::Sender<AcmeChallengeEvent>>,
    sync_status: SyncStatus,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(StoreInner {
                machines: HashMap::new(),
                machine_subscribers: Vec::new(),
                routing_subscribers: Vec::new(),
                invites: HashMap::new(),
                service_revisions: HashMap::new(),
                service_releases: HashMap::new(),
                volumes: HashMap::new(),
                instance_status: HashMap::new(),
                deploys: HashMap::new(),
                acme_accounts: HashMap::new(),
                certificates: HashMap::new(),
                certificate_subscribers: Vec::new(),
                acme_challenges: HashMap::new(),
                acme_challenge_subscribers: Vec::new(),
                sync_status: SyncStatus::Synced,
            }),
        }
    }

    pub fn set_sync_status(&self, status: SyncStatus) {
        self.lock_inner().sync_status = status;
    }

    fn lock_inner(&self) -> MutexGuard<'_, StoreInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn broadcast_machine(inner: &mut StoreInner, event: MachineEvent) {
        inner
            .machine_subscribers
            .retain(|sender| match sender.try_send(event.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Closed(_)) => false,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!("subscriber channel full, event dropped");
                    true
                }
            });
    }

    fn broadcast_routing_refresh(inner: &mut StoreInner) {
        inner
            .routing_subscribers
            .retain(|sender| match sender.try_send(()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Closed(_)) => false,
                Err(mpsc::error::TrySendError::Full(_)) => true,
            });
    }

    fn broadcast_certificate(inner: &mut StoreInner, event: CertificateEvent) {
        inner
            .certificate_subscribers
            .retain(|sender| match sender.try_send(event.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Closed(_)) => false,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!("certificate subscriber channel full, event dropped");
                    true
                }
            });
    }

    fn broadcast_acme_challenge(inner: &mut StoreInner, event: AcmeChallengeEvent) {
        inner
            .acme_challenge_subscribers
            .retain(|sender| match sender.try_send(event.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Closed(_)) => false,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!("acme challenge subscriber channel full, event dropped");
                    true
                }
            });
    }
}

impl SyncProbe for MemoryStore {
    async fn sync_status(&self) -> Result<SyncStatus> {
        Ok(self.lock_inner().sync_status)
    }
}

impl MachineRegistry for MemoryStore {
    async fn list_machines(&self) -> Result<Vec<MachineMembership>> {
        let inner = self.lock_inner();
        Ok(inner.machines.values().cloned().collect())
    }

    async fn upsert_self_machine(&self, record: &MachineMembership) -> Result<()> {
        let mut inner = self.lock_inner();
        let is_update = inner.machines.contains_key(&record.id);
        inner.machines.insert(record.id.clone(), record.clone());
        let event = if is_update {
            MachineEvent::Updated(record.clone())
        } else {
            MachineEvent::Added(record.clone())
        };
        Self::broadcast_machine(&mut inner, event);
        Self::broadcast_routing_refresh(&mut inner);
        Ok(())
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        let mut inner = self.lock_inner();
        if let Some(record) = inner.machines.remove(id) {
            Self::broadcast_machine(&mut inner, MachineEvent::Removed(record));
            Self::broadcast_routing_refresh(&mut inner);
        }
        Ok(())
    }

    async fn subscribe_machines(&self) -> Result<crate::MachineSubscription> {
        let mut inner = self.lock_inner();
        let snapshot = inner.machines.values().cloned().collect();
        let (sender, receiver) = mpsc::channel(64);
        inner.machine_subscribers.push(sender);
        Ok((snapshot, receiver))
    }
}

impl RoutingSnapshotReader for MemoryStore {
    async fn load_routing_state(&self) -> Result<RoutingState> {
        let inner = self.lock_inner();
        Ok(RoutingState {
            machines: inner.machines.values().cloned().collect(),
            revisions: inner.service_revisions.values().cloned().collect(),
            releases: inner.service_releases.values().cloned().collect(),
            instances: inner.instance_status.values().cloned().collect(),
        })
    }

    async fn subscribe_routing_invalidations(&self) -> Result<RoutingInvalidationSubscription> {
        let mut inner = self.lock_inner();
        let (sender, receiver) = mpsc::channel(64);
        inner.routing_subscribers.push(sender);
        Ok(receiver)
    }
}

impl InviteRepository for MemoryStore {
    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        let mut inner = self.lock_inner();
        if inner.invites.contains_key(&invite.invite_id) {
            return Err(Error::operation(
                "invite_exists",
                format!("invite '{}' already exists", invite.invite_id),
            ));
        }
        inner
            .invites
            .insert(invite.invite_id.clone(), invite.clone());
        Ok(())
    }

    async fn get_invite(&self, invite_id: &str) -> Result<Option<InviteRecord>> {
        let inner = self.lock_inner();
        Ok(inner.invites.get(invite_id).cloned())
    }

    async fn list_invites(&self) -> Result<Vec<InviteRecord>> {
        let inner = self.lock_inner();
        let mut invites = inner.invites.values().cloned().collect::<Vec<_>>();
        invites.sort_by(|left, right| left.invite_id.cmp(&right.invite_id));
        Ok(invites)
    }

    async fn redeem_invite(
        &self,
        invite_id: &str,
        machine_id: &MachineId,
        now_unix_secs: u64,
    ) -> Result<InviteRecord> {
        let mut inner = self.lock_inner();
        let Some(invite) = inner.invites.get(invite_id).cloned() else {
            return Err(Error::operation(
                "invite_not_found",
                format!("invite '{invite_id}' not found"),
            ));
        };

        if invite.revoked_at.is_some() {
            return Err(Error::operation(
                "invite_revoked",
                format!("invite '{invite_id}' is revoked"),
            ));
        }

        if now_unix_secs > invite.expires_at {
            return Err(Error::operation(
                "invite_expired",
                format!("invite '{invite_id}' is expired"),
            ));
        }

        if let Some(consumed_by) = &invite.consumed_by {
            if consumed_by == machine_id {
                return Ok(invite);
            }
            return Err(Error::operation(
                "invite_consumed",
                format!("invite '{invite_id}' is already consumed"),
            ));
        }

        let mut next_invite = invite.clone();
        next_invite.consumed_by = Some(machine_id.clone());
        next_invite.consumed_at = Some(now_unix_secs);
        inner
            .invites
            .insert(invite_id.to_string(), next_invite.clone());

        Ok(next_invite)
    }

    async fn revoke_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<InviteRecord> {
        let mut inner = self.lock_inner();
        let invite = inner.invites.get(invite_id).ok_or_else(|| {
            Error::operation(
                "invite_not_found",
                format!("invite '{invite_id}' not found"),
            )
        })?;
        if invite.consumed_by.is_some() {
            return Err(Error::operation(
                "invite_consumed",
                format!("invite '{invite_id}' is already consumed"),
            ));
        }

        let mut next_invite = invite.clone();
        next_invite.revoked_at = Some(now_unix_secs);
        inner
            .invites
            .insert(invite_id.to_string(), next_invite.clone());
        Ok(next_invite)
    }
}

impl DeployRepository for MemoryStore {
    async fn list_deploy_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        let inner = self.lock_inner();
        Ok(Self::list_deploy_releases_inner(&inner, namespace))
    }

    async fn load_deploy_snapshot(&self, namespace: &Namespace) -> Result<DeploySnapshot> {
        let inner = self.lock_inner();
        let revisions = inner
            .service_revisions
            .values()
            .filter(|record| record.namespace == *namespace)
            .cloned()
            .collect();
        let releases = Self::list_deploy_releases_inner(&inner, namespace);
        let instances = inner
            .instance_status
            .values()
            .filter(|record| record.namespace == *namespace)
            .cloned()
            .collect();
        Ok(DeploySnapshot {
            revisions,
            releases,
            instances,
        })
    }

    async fn list_volumes(&self, namespace: &Namespace) -> Result<Vec<VolumeRecord>> {
        let inner = self.lock_inner();
        let mut volumes = inner
            .volumes
            .values()
            .filter(|record| record.namespace == *namespace)
            .cloned()
            .collect::<Vec<_>>();
        volumes.sort_by(|left, right| left.volume_name.cmp(&right.volume_name));
        Ok(volumes)
    }

    async fn get_volume(
        &self,
        namespace: &Namespace,
        volume_name: &str,
    ) -> Result<Option<VolumeRecord>> {
        let inner = self.lock_inner();
        Ok(inner
            .volumes
            .get(&(namespace.clone(), volume_name.to_string()))
            .cloned())
    }

    async fn record_service_revision(&self, command: &DeployRevisionUpsert) -> Result<()> {
        let mut inner = self.lock_inner();
        Self::record_service_revision_inner(&mut inner, &command.revision);
        Self::broadcast_routing_refresh(&mut inner);
        Ok(())
    }

    async fn commit_deploy(&self, command: &DeployCommit) -> Result<()> {
        let mut inner = self.lock_inner();
        Self::commit_deploy_inner(&mut inner, command);
        Self::broadcast_routing_refresh(&mut inner);
        Ok(())
    }

    async fn update_deploy_record(&self, command: &DeployRecordUpdate) -> Result<()> {
        let mut inner = self.lock_inner();
        inner
            .deploys
            .insert(command.deploy.deploy_id.clone(), command.deploy.clone());
        Ok(())
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        let inner = self.lock_inner();
        Ok(inner.deploys.get(deploy_id).cloned())
    }
}

impl InstanceStatusRepository for MemoryStore {
    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        let inner = self.lock_inner();
        Ok(inner
            .instance_status
            .values()
            .filter(|record| record.namespace == *namespace)
            .cloned()
            .collect())
    }

    async fn record_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        let mut inner = self.lock_inner();
        Self::record_instance_status_inner(&mut inner, record);
        Self::broadcast_routing_refresh(&mut inner);
        Ok(())
    }

    async fn remove_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        let mut inner = self.lock_inner();
        Self::remove_instance_status_inner(&mut inner, instance_id);
        Self::broadcast_routing_refresh(&mut inner);
        Ok(())
    }
}

impl MemoryStore {
    fn list_deploy_releases_inner(
        inner: &StoreInner,
        namespace: &Namespace,
    ) -> Vec<ServiceReleaseRecord> {
        inner
            .service_releases
            .values()
            .filter(|record| record.namespace == *namespace)
            .cloned()
            .collect()
    }

    fn record_service_revision_inner(inner: &mut StoreInner, record: &ServiceRevisionRecord) {
        let key = (
            record.namespace.clone(),
            record.service.clone(),
            record.revision_hash.clone(),
        );
        inner.service_revisions.insert(key, record.clone());
    }

    fn record_instance_status_inner(inner: &mut StoreInner, record: &InstanceStatusRecord) {
        inner
            .instance_status
            .insert(record.instance_id.clone(), record.clone());
    }

    fn remove_instance_status_inner(inner: &mut StoreInner, instance_id: &InstanceId) {
        inner.instance_status.remove(instance_id);
    }

    fn commit_deploy_inner(inner: &mut StoreInner, command: &DeployCommit) {
        let touched_services: HashSet<&str> = command
            .removed_services
            .iter()
            .map(String::as_str)
            .chain(
                command
                    .releases
                    .iter()
                    .map(|record| record.service.as_str()),
            )
            .collect();

        inner
            .service_releases
            .retain(|(current_namespace, service), _| {
                !(current_namespace == &command.namespace
                    && touched_services.contains(service.as_str()))
            });

        for release in &command.releases {
            inner.service_releases.insert(
                (release.namespace.clone(), release.service.clone()),
                release.clone(),
            );
        }

        for volume in &command.volumes {
            inner.volumes.insert(
                (volume.namespace.clone(), volume.volume_name.clone()),
                volume.clone(),
            );
        }

        for volume_name in &command.removed_volumes {
            inner
                .volumes
                .remove(&(command.namespace.clone(), volume_name.clone()));
        }

        inner
            .deploys
            .insert(command.deploy.deploy_id.clone(), command.deploy.clone());
    }
}

impl CertificateStore for MemoryStore {
    async fn get_acme_account(&self, issuer_url: &str) -> Result<Option<AcmeAccountRecord>> {
        let inner = self.lock_inner();
        Ok(inner.acme_accounts.get(issuer_url).cloned())
    }

    async fn upsert_acme_account(&self, record: &AcmeAccountRecord) -> Result<()> {
        let mut inner = self.lock_inner();
        inner
            .acme_accounts
            .insert(record.issuer_url.clone(), record.clone());
        Self::broadcast_routing_refresh(&mut inner);
        Ok(())
    }

    async fn list_certificates(&self) -> Result<Vec<CertificateRecord>> {
        let inner = self.lock_inner();
        Ok(inner.certificates.values().cloned().collect())
    }

    async fn get_certificate(&self, hostname: &str) -> Result<Option<CertificateRecord>> {
        let inner = self.lock_inner();
        Ok(inner.certificates.get(hostname).cloned())
    }

    async fn upsert_certificate(&self, record: &CertificateRecord) -> Result<()> {
        let mut inner = self.lock_inner();
        let is_update = inner.certificates.contains_key(&record.hostname);
        inner
            .certificates
            .insert(record.hostname.clone(), record.clone());
        let event = if is_update {
            CertificateEvent::Updated(record.clone())
        } else {
            CertificateEvent::Added(record.clone())
        };
        Self::broadcast_certificate(&mut inner, event);
        Self::broadcast_routing_refresh(&mut inner);
        Ok(())
    }

    async fn list_acme_challenges(&self) -> Result<Vec<AcmeChallengeRecord>> {
        let inner = self.lock_inner();
        Ok(inner.acme_challenges.values().cloned().collect())
    }

    async fn upsert_acme_challenge(&self, record: &AcmeChallengeRecord) -> Result<()> {
        let mut inner = self.lock_inner();
        let key = (record.hostname.clone(), record.token.clone());
        let is_update = inner.acme_challenges.contains_key(&key);
        inner.acme_challenges.insert(key, record.clone());
        let event = if is_update {
            AcmeChallengeEvent::Updated(record.clone())
        } else {
            AcmeChallengeEvent::Added(record.clone())
        };
        Self::broadcast_acme_challenge(&mut inner, event);
        Self::broadcast_routing_refresh(&mut inner);
        Ok(())
    }

    async fn delete_acme_challenge(&self, hostname: &str, token: &str) -> Result<()> {
        let mut inner = self.lock_inner();
        if let Some(record) = inner
            .acme_challenges
            .remove(&(hostname.to_string(), token.to_string()))
        {
            Self::broadcast_acme_challenge(&mut inner, AcmeChallengeEvent::Removed(record));
        }
        Self::broadcast_routing_refresh(&mut inner);
        Ok(())
    }

    async fn subscribe_certificates(&self) -> Result<CertificateSubscription> {
        let mut inner = self.lock_inner();
        let snapshot = inner.certificates.values().cloned().collect();
        let (sender, receiver) = mpsc::channel(64);
        inner.certificate_subscribers.push(sender);
        Ok((snapshot, receiver))
    }

    async fn subscribe_acme_challenges(&self) -> Result<AcmeChallengeSubscription> {
        let mut inner = self.lock_inner();
        let snapshot = inner.acme_challenges.values().cloned().collect();
        let (sender, receiver) = mpsc::channel(64);
        inner.acme_challenge_subscribers.push(sender);
        Ok((snapshot, receiver))
    }
}

impl MemoryStore {
    pub async fn wipe_data(&self) -> Result<()> {
        let mut inner = self.lock_inner();
        let removed = inner
            .machines
            .drain()
            .map(|(_, record)| record)
            .collect::<Vec<_>>();
        inner.invites.clear();
        inner.service_revisions.clear();
        inner.service_releases.clear();
        inner.volumes.clear();
        inner.instance_status.clear();
        inner.deploys.clear();
        inner.acme_accounts.clear();
        inner.certificates.clear();
        inner.acme_challenges.clear();

        for record in removed {
            Self::broadcast_machine(&mut inner, MachineEvent::Removed(record));
        }
        Self::broadcast_routing_refresh(&mut inner);
        Ok(())
    }
}

pub struct MemoryService {
    started: AtomicBool,
    healthy: AtomicBool,
    fail_start: AtomicBool,
    fail_stop: AtomicBool,
}

impl Default for MemoryService {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryService {
    #[must_use]
    pub fn new() -> Self {
        Self {
            started: AtomicBool::new(false),
            healthy: AtomicBool::new(true),
            fail_start: AtomicBool::new(false),
            fail_stop: AtomicBool::new(false),
        }
    }

    pub fn set_healthy(&self, healthy: bool) {
        self.healthy.store(healthy, Ordering::SeqCst);
    }

    pub fn set_fail_start(&self, fail: bool) {
        self.fail_start.store(fail, Ordering::SeqCst);
    }

    pub fn set_fail_stop(&self, fail: bool) {
        self.fail_stop.store(fail, Ordering::SeqCst);
    }

    pub fn is_started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl StoreRuntimeControl for MemoryService {
    async fn start(&self) -> Result<()> {
        if self.fail_start.load(Ordering::SeqCst) {
            return Err(Error::operation("service start", "injected failure"));
        }
        self.started.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        if self.fail_stop.load(Ordering::SeqCst) {
            return Err(Error::operation("service stop", "injected failure"));
        }
        self.started.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn healthy(&self) -> bool {
        self.healthy.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invite_is_single_use() {
        let store = MemoryStore::new();
        let invite = InviteRecord {
            invite_id: "inv-1".into(),
            network_id: ployz_types::model::NetworkId("net-1".into()),
            issuer_machine_id: MachineId("issuer".into()),
            issuer_verify_key: "verify".into(),
            expires_at: 10_000,
            consumed_by: None,
            consumed_at: None,
            revoked_at: None,
            signature: "sig".into(),
        };

        store.create_invite(&invite).await.expect("create invite");
        store
            .redeem_invite("inv-1", &MachineId("joiner".into()), 100)
            .await
            .expect("consume invite once");

        let second = store
            .redeem_invite("inv-1", &MachineId("other".into()), 101)
            .await;
        assert!(matches!(
            second,
            Err(Error::Operation {
                operation: "invite_consumed",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn invite_expiry_is_enforced() {
        let store = MemoryStore::new();
        let invite = InviteRecord {
            invite_id: "inv-2".into(),
            network_id: ployz_types::model::NetworkId("net-1".into()),
            issuer_machine_id: MachineId("issuer".into()),
            issuer_verify_key: "verify".into(),
            expires_at: 50,
            consumed_by: None,
            consumed_at: None,
            revoked_at: None,
            signature: "sig".into(),
        };

        store.create_invite(&invite).await.expect("create invite");

        let expired = store
            .redeem_invite("inv-2", &MachineId("joiner".into()), 51)
            .await;
        assert!(matches!(
            expired,
            Err(Error::Operation {
                operation: "invite_expired",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn record_service_revision_updates_routing_snapshot_and_emits_refresh() {
        let store = MemoryStore::new();
        let mut refresh_rx = store
            .subscribe_routing_invalidations()
            .await
            .expect("subscribe");

        let namespace = Namespace("prod".into());
        let revision = ServiceRevisionRecord {
            namespace: namespace.clone(),
            service: "api".into(),
            revision_hash: "rev-1".into(),
            spec_json: "{}".into(),
            created_by: MachineId("machine-1".into()),
            created_at: 1,
        };

        store
            .record_service_revision(&DeployRevisionUpsert { revision })
            .await
            .expect("record revision");

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), refresh_rx.recv())
            .await
            .expect("refresh event deadline");
        assert_eq!(event, Some(()));

        let snapshot = store
            .load_routing_state()
            .await
            .expect("load routing state");
        assert_eq!(snapshot.revisions.len(), 1);
    }

    #[tokio::test]
    async fn commit_deploy_replaces_touched_releases_and_records_deploy() {
        let store = MemoryStore::new();
        let namespace = Namespace("prod".into());
        let untouched = test_release(&namespace, "worker", "rev-old", "deploy-old");
        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                releases: vec![
                    test_release(&namespace, "api", "rev-old", "deploy-old"),
                    untouched.clone(),
                ],
                volumes: Vec::new(),
                deploy: test_deploy(&namespace, "deploy-old"),
            })
            .await
            .expect("seed deploy");

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                removed_services: vec!["worker".into()],
                removed_volumes: Vec::new(),
                releases: vec![test_release(&namespace, "api", "rev-new", "deploy-new")],
                volumes: Vec::new(),
                deploy: test_deploy(&namespace, "deploy-new"),
            })
            .await
            .expect("commit deploy");

        let snapshot = store
            .load_deploy_snapshot(&namespace)
            .await
            .expect("load deploy snapshot");
        assert_eq!(snapshot.releases.len(), 1);
        assert_eq!(snapshot.releases[0].service, "api");
        assert_eq!(
            snapshot.releases[0].release.primary_revision_hash,
            "rev-new"
        );
        assert!(
            store
                .get_deploy(&DeployId("deploy-new".into()))
                .await
                .expect("get deploy")
                .is_some()
        );
    }

    #[tokio::test]
    async fn instance_status_writes_update_routing_snapshot_and_emit_refreshes() {
        let store = MemoryStore::new();
        let namespace = Namespace("prod".into());
        let status = test_instance_status(&namespace, "inst-1");
        let mut refresh_rx = store
            .subscribe_routing_invalidations()
            .await
            .expect("subscribe");

        store
            .record_instance_status(&status)
            .await
            .expect("record instance status");
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), refresh_rx.recv())
            .await
            .expect("record refresh deadline");
        assert_eq!(event, Some(()));

        store
            .remove_instance_status(&status.instance_id)
            .await
            .expect("remove instance status");
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), refresh_rx.recv())
            .await
            .expect("remove refresh deadline");
        assert_eq!(event, Some(()));

        let snapshot = store
            .load_routing_state()
            .await
            .expect("load routing state");
        assert!(snapshot.instances.is_empty());
    }

    fn test_release(
        namespace: &Namespace,
        service: &str,
        revision_hash: &str,
        deploy_id: &str,
    ) -> ServiceReleaseRecord {
        ServiceReleaseRecord {
            namespace: namespace.clone(),
            service: service.into(),
            release: ployz_types::model::ServiceRelease {
                primary_revision_hash: revision_hash.into(),
                referenced_revision_hashes: vec![revision_hash.into()],
                routing: ployz_types::model::ServiceRoutingPolicy::Direct {
                    revision_hash: revision_hash.into(),
                },
                slots: Vec::new(),
                updated_by_deploy_id: DeployId(deploy_id.into()),
                updated_at: 1,
            },
        }
    }

    fn test_deploy(namespace: &Namespace, deploy_id: &str) -> DeployRecord {
        DeployRecord {
            deploy_id: DeployId(deploy_id.into()),
            namespace: namespace.clone(),
            coordinator_machine_id: MachineId("machine-1".into()),
            manifest_hash: "manifest".into(),
            state: ployz_types::model::DeployState::Committed,
            started_at: 1,
            committed_at: Some(2),
            finished_at: Some(2),
            summary_json: "{}".into(),
        }
    }

    fn test_instance_status(namespace: &Namespace, instance_id: &str) -> InstanceStatusRecord {
        InstanceStatusRecord {
            instance_id: InstanceId(instance_id.into()),
            namespace: namespace.clone(),
            service: "api".into(),
            slot_id: ployz_types::model::SlotId("slot-1".into()),
            machine_id: MachineId("machine-1".into()),
            revision_hash: "rev-1".into(),
            deploy_id: DeployId("deploy-1".into()),
            docker_container_id: "container-1".into(),
            overlay_ip: None,
            backend_ports: std::collections::BTreeMap::new(),
            phase: ployz_types::model::InstancePhase::Ready,
            ready: true,
            drain_state: ployz_types::model::DrainState::None,
            error: None,
            started_at: 1,
            updated_at: 1,
        }
    }

    #[tokio::test]
    async fn load_routing_state_includes_machines() {
        let store = MemoryStore::new();
        let machine = MachineMembership {
            id: MachineId("machine-1".into()),
            public_key: ployz_types::model::PublicKey([0; 32]),
            overlay_ip: ployz_types::model::OverlayIp("fd00::1".parse().expect("valid overlay")),
            topology: ployz_types::model::MachineTopology::new("us-east", Some("use1-a"))
                .expect("topology should parse"),
            control_target: None,
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle: ployz_types::model::MachineLifecycle::Active,
            created_at: 1,
            updated_at: 1,
            labels: std::collections::BTreeMap::new(),
        };

        store
            .upsert_self_machine(&machine)
            .await
            .expect("upsert machine");

        let state = store.load_routing_state().await.expect("routing state");

        assert_eq!(state.machines, vec![machine]);
    }

    #[tokio::test]
    async fn machine_changes_trigger_routing_refresh_events() {
        let store = MemoryStore::new();
        let mut refresh_rx = store
            .subscribe_routing_invalidations()
            .await
            .expect("subscribe");
        let machine = MachineMembership {
            id: MachineId("machine-1".into()),
            public_key: ployz_types::model::PublicKey([0; 32]),
            overlay_ip: ployz_types::model::OverlayIp("fd00::1".parse().expect("valid overlay")),
            topology: ployz_types::model::MachineTopology::local(),
            control_target: None,
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle: ployz_types::model::MachineLifecycle::Active,
            created_at: 1,
            updated_at: 1,
            labels: std::collections::BTreeMap::new(),
        };

        store
            .upsert_self_machine(&machine)
            .await
            .expect("upsert machine");

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), refresh_rx.recv())
            .await
            .expect("refresh event deadline");
        assert_eq!(event, Some(()));
    }

    #[tokio::test]
    async fn wipe_data_clears_volume_records() {
        let store = MemoryStore::new();
        let namespace = Namespace("prod".into());
        let deploy_id = DeployId("dep-1".into());
        let volume = VolumeRecord {
            namespace: namespace.clone(),
            volume_name: "data".into(),
            scope: ployz_types::spec::VolumeScope::Single,
            machine_id: MachineId("machine-1".into()),
            quota: "1G".into(),
            mode: "0750".into(),
            owner: "999:999".into(),
            attached_services: Vec::new(),
            created_at: 1,
            created_by_deploy_id: deploy_id.clone(),
            last_modified_at: 1,
            last_modified_by_deploy_id: deploy_id.clone(),
        };
        let deploy = DeployRecord {
            deploy_id,
            namespace: namespace.clone(),
            coordinator_machine_id: MachineId("local".into()),
            manifest_hash: "hash".into(),
            state: ployz_types::model::DeployState::Committed,
            started_at: 1,
            committed_at: Some(1),
            finished_at: Some(1),
            summary_json: "{}".into(),
        };

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                releases: Vec::new(),
                volumes: vec![volume],
                deploy,
            })
            .await
            .expect("commit volume");
        assert_eq!(
            store
                .list_volumes(&namespace)
                .await
                .expect("list volumes before wipe")
                .len(),
            1
        );

        store.wipe_data().await.expect("wipe data");

        assert!(
            store
                .list_volumes(&namespace)
                .await
                .expect("list volumes after wipe")
                .is_empty()
        );
    }
}
