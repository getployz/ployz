use crate::{
    AcmeChallengeSubscription, CertificateStore, CertificateSubscription, DeployStore,
    InviteStore, MachineStore, RoutingInvalidationSubscription, RoutingStore, StoreRuntimeControl,
    SyncProbe, SyncStatus,
};
use async_trait::async_trait;
use ployz_types::error::{Error, Result};
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeEvent, AcmeChallengeRecord, CertificateEvent,
    CertificateRecord, DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord,
    MachineEvent, MachineId, MachineRecord, RoutingState, ServiceReleaseRecord,
    ServiceRevisionRecord,
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
    machines: HashMap<MachineId, MachineRecord>,
    machine_subscribers: Vec<mpsc::Sender<MachineEvent>>,
    routing_subscribers: Vec<mpsc::Sender<()>>,
    invites: HashMap<String, InviteRecord>,
    service_revisions: HashMap<(Namespace, String, String), ServiceRevisionRecord>,
    service_releases: HashMap<(Namespace, String), ServiceReleaseRecord>,
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

impl MachineStore for MemoryStore {
    async fn list_machines(&self) -> Result<Vec<MachineRecord>> {
        let inner = self.lock_inner();
        Ok(inner.machines.values().cloned().collect())
    }

    async fn upsert_self_machine(&self, record: &MachineRecord) -> Result<()> {
        let mut inner = self.lock_inner();
        let is_update = inner.machines.contains_key(&record.id);
        inner.machines.insert(record.id.clone(), record.clone());
        let event = if is_update {
            MachineEvent::Updated(record.clone())
        } else {
            MachineEvent::Added(record.clone())
        };
        Self::broadcast_machine(&mut inner, event);
        Ok(())
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        let mut inner = self.lock_inner();
        if let Some(record) = inner.machines.remove(id) {
            Self::broadcast_machine(&mut inner, MachineEvent::Removed(record));
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

impl RoutingStore for MemoryStore {
    async fn load_routing_state(&self) -> Result<RoutingState> {
        let inner = self.lock_inner();
        Ok(RoutingState {
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

impl InviteStore for MemoryStore {
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

impl DeployStore for MemoryStore {
    async fn list_service_revisions(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceRevisionRecord>> {
        let inner = self.lock_inner();
        Ok(inner
            .service_revisions
            .values()
            .filter(|record| record.namespace == *namespace)
            .cloned()
            .collect())
    }

    async fn list_service_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        let inner = self.lock_inner();
        Ok(inner
            .service_releases
            .values()
            .filter(|record| record.namespace == *namespace)
            .cloned()
            .collect())
    }

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

    async fn upsert_service_revision(&self, record: &ServiceRevisionRecord) -> Result<()> {
        let mut inner = self.lock_inner();
        let key = (
            record.namespace.clone(),
            record.service.clone(),
            record.revision_hash.clone(),
        );
        inner.service_revisions.insert(key, record.clone());
        Self::broadcast_routing_refresh(&mut inner);
        Ok(())
    }

    async fn upsert_service_release(&self, record: &ServiceReleaseRecord) -> Result<()> {
        let mut inner = self.lock_inner();
        let key = (record.namespace.clone(), record.service.clone());
        inner.service_releases.insert(key, record.clone());
        Self::broadcast_routing_refresh(&mut inner);
        Ok(())
    }

    async fn delete_service_release(&self, namespace: &Namespace, service: &str) -> Result<()> {
        let mut inner = self.lock_inner();
        inner
            .service_releases
            .remove(&(namespace.clone(), service.to_string()));
        Self::broadcast_routing_refresh(&mut inner);
        Ok(())
    }

    async fn upsert_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        let mut inner = self.lock_inner();
        inner
            .instance_status
            .insert(record.instance_id.clone(), record.clone());
        Self::broadcast_routing_refresh(&mut inner);
        Ok(())
    }

    async fn delete_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        let mut inner = self.lock_inner();
        inner.instance_status.remove(instance_id);
        Self::broadcast_routing_refresh(&mut inner);
        Ok(())
    }

    async fn upsert_deploy(&self, record: &DeployRecord) -> Result<()> {
        let mut inner = self.lock_inner();
        inner
            .deploys
            .insert(record.deploy_id.clone(), record.clone());
        Ok(())
    }

    async fn commit_deploy(
        &self,
        namespace: &Namespace,
        removed_services: &[String],
        releases: &[ServiceReleaseRecord],
        deploy: &DeployRecord,
    ) -> Result<()> {
        let mut inner = self.lock_inner();
        let touched_services: HashSet<&str> = removed_services
            .iter()
            .map(String::as_str)
            .chain(releases.iter().map(|record| record.service.as_str()))
            .collect();

        inner
            .service_releases
            .retain(|(current_namespace, service), _| {
                !(current_namespace == namespace && touched_services.contains(service.as_str()))
            });

        for release in releases {
            inner.service_releases.insert(
                (release.namespace.clone(), release.service.clone()),
                release.clone(),
            );
        }

        inner
            .deploys
            .insert(deploy.deploy_id.clone(), deploy.clone());
        Self::broadcast_routing_refresh(&mut inner);
        Ok(())
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        let inner = self.lock_inner();
        Ok(inner.deploys.get(deploy_id).cloned())
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
    async fn routing_subscribers_receive_refresh_events() {
        let store = MemoryStore::new();
        let mut refresh_rx = store
            .subscribe_routing_invalidations()
            .await
            .expect("subscribe");

        let namespace = Namespace("prod".into());
        let record = ServiceReleaseRecord {
            namespace,
            service: "api".into(),
            release: ployz_types::model::ServiceRelease {
                primary_revision_hash: "rev-1".into(),
                referenced_revision_hashes: vec!["rev-1".into()],
                routing: ployz_types::model::ServiceRoutingPolicy::Direct {
                    revision_hash: "rev-1".into(),
                },
                slots: Vec::new(),
                updated_by_deploy_id: DeployId("dep-1".into()),
                updated_at: 1,
            },
        };

        store
            .upsert_service_release(&record)
            .await
            .expect("upsert service release");

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), refresh_rx.recv())
            .await
            .expect("refresh event deadline");
        assert_eq!(event, Some(()));
    }
}
