use crate::{
    AcmeChallengeSubscription, CertificateStore, CertificateSubscription, DeployCommit,
    DeployRecordUpdate, DeployRepository, DeployRevisionUpsert, DeploySnapshot,
    InstanceStatusRepository, InviteRepository, MachineRegistry, RoutingBatchSubscription,
    RoutingEventBatch, RoutingSnapshotReader, StoreRuntimeControl, SyncProbe, SyncStatus,
};
use async_trait::async_trait;
use ployz_types::error::{Error, Result};
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeEvent, AcmeChallengeReadinessRecord, AcmeChallengeRecord,
    CertificateEvent, CertificateRecord, DeployId, DeployRecord, InstanceId, InstanceStatusRecord,
    InviteRecord, MachineEvent, MachineId, MachineMembership, RoutingEvent, RoutingState,
    ServiceReleaseRecord, ServiceRevisionRecord, VolumeRecord,
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
    machine_subscribers: Vec<mpsc::Sender<crate::MachineSubscriptionUpdate>>,
    routing_subscribers: Vec<mpsc::Sender<crate::RoutingBatchSubscriptionUpdate>>,
    invites: HashMap<String, InviteRecord>,
    service_revisions: HashMap<(Namespace, String, String), ServiceRevisionRecord>,
    service_releases: HashMap<(Namespace, String), ServiceReleaseRecord>,
    volumes: HashMap<(Namespace, String), VolumeRecord>,
    instance_status: HashMap<InstanceId, InstanceStatusRecord>,
    deploys: HashMap<DeployId, DeployRecord>,
    acme_accounts: HashMap<String, AcmeAccountRecord>,
    certificates: HashMap<String, CertificateRecord>,
    certificate_subscribers: Vec<mpsc::Sender<crate::CertificateSubscriptionUpdate>>,
    acme_challenges: HashMap<(String, String), AcmeChallengeRecord>,
    acme_challenge_subscribers: Vec<mpsc::Sender<crate::AcmeChallengeSubscriptionUpdate>>,
    acme_challenge_readiness: HashMap<(String, String, MachineId), AcmeChallengeReadinessRecord>,
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
                acme_challenge_readiness: HashMap::new(),
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
            .retain(|sender| match sender.try_send(Ok(event.clone())) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Closed(_)) => false,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!("machine subscriber channel full, closing stale event stream");
                    false
                }
            });
    }

    fn broadcast_routing_batch(
        inner: &mut StoreInner,
        batch_id: impl Into<String> + Clone,
        cause: Option<String>,
        events: Vec<RoutingEvent>,
    ) {
        if events.is_empty() {
            return;
        }
        inner.routing_subscribers.retain(|sender| {
            match sender.try_send(Ok(RoutingEventBatch::unacked(
                batch_id.clone(),
                cause.clone(),
                events.clone(),
            ))) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Closed(_)) => false,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!("routing subscriber channel full, closing stale delta stream");
                    false
                }
            }
        });
    }

    fn routing_state(inner: &StoreInner) -> RoutingState {
        RoutingState {
            machines: inner.machines.values().cloned().collect(),
            revisions: inner.service_revisions.values().cloned().collect(),
            releases: inner.service_releases.values().cloned().collect(),
            instances: inner.instance_status.values().cloned().collect(),
        }
    }

    pub async fn load_routing_state(&self) -> Result<RoutingState> {
        let inner = self.lock_inner();
        Ok(Self::routing_state(&inner))
    }

    pub async fn subscribe_routing_events(
        &self,
    ) -> Result<(RoutingState, mpsc::Receiver<RoutingEvent>)> {
        let (state, mut batches) = self
            .subscribe_routing_batches(crate::RoutingSubscription::temporary("memory.events"))
            .await?;
        let (tx, rx) = mpsc::channel(1024);
        tokio::spawn(async move {
            while let Some(batch) = batches.recv().await {
                let Ok(batch) = batch else {
                    return;
                };
                for event in batch.events {
                    if tx.send(event).await.is_err() {
                        return;
                    }
                }
            }
        });
        Ok((state, rx))
    }

    fn broadcast_certificate(inner: &mut StoreInner, event: CertificateEvent) {
        inner
            .certificate_subscribers
            .retain(|sender| match sender.try_send(Ok(event.clone())) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Closed(_)) => false,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!("certificate subscriber channel full, closing stale event stream");
                    false
                }
            });
    }

    fn broadcast_acme_challenge(inner: &mut StoreInner, event: AcmeChallengeEvent) {
        inner.acme_challenge_subscribers.retain(|sender| {
            match sender.try_send(Ok(event.clone())) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Closed(_)) => false,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!("acme challenge subscriber channel full, closing stale event stream");
                    false
                }
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
        let old = inner.machines.insert(record.id.clone(), record.clone());
        let machine_event = if is_update {
            MachineEvent::Updated(record.clone())
        } else {
            MachineEvent::Added(record.clone())
        };
        let routing_event = match old {
            Some(old) => RoutingEvent::MachineUpdated {
                old,
                new: record.clone(),
            },
            None => RoutingEvent::MachineAdded(record.clone()),
        };
        Self::broadcast_machine(&mut inner, machine_event);
        Self::broadcast_routing_batch(
            &mut inner,
            format!("memory:machine:{}", record.id),
            Some("machine.upsert".to_string()),
            vec![routing_event],
        );
        Ok(())
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        let mut inner = self.lock_inner();
        if let Some(record) = inner.machines.remove(id) {
            Self::broadcast_machine(&mut inner, MachineEvent::Removed(record.clone()));
            Self::broadcast_routing_batch(
                &mut inner,
                format!("memory:machine:delete:{id}"),
                Some("machine.delete".to_string()),
                vec![RoutingEvent::MachineRemoved(record)],
            );
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
        self.load_routing_state().await
    }

    async fn subscribe_routing_batches(
        &self,
        _subscription: crate::RoutingSubscription,
    ) -> Result<RoutingBatchSubscription> {
        let mut inner = self.lock_inner();
        let state = Self::routing_state(&inner);
        let (sender, receiver) = mpsc::channel(1024);
        inner.routing_subscribers.push(sender);
        Ok((state, receiver))
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
        let old = Self::record_service_revision_inner(&mut inner, &command.revision);
        if let Some(event) = revision_event(old, &command.revision) {
            Self::broadcast_routing_batch(
                &mut inner,
                format!(
                    "memory:revision:{}:{}:{}",
                    command.revision.namespace,
                    command.revision.service,
                    command.revision.revision_hash
                ),
                Some("deploy.revision".to_string()),
                vec![event],
            );
        }
        Ok(())
    }

    async fn commit_deploy(&self, command: &DeployCommit) -> Result<()> {
        let mut inner = self.lock_inner();
        let events = Self::commit_deploy_inner(&mut inner, command);
        Self::broadcast_routing_batch(
            &mut inner,
            format!("memory:deploy:{}", command.deploy.deploy_id),
            Some("deploy.commit".to_string()),
            events,
        );
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
        let old = Self::record_instance_status_inner(&mut inner, record);
        let event = match old {
            Some(old) => RoutingEvent::InstanceUpdated {
                old,
                new: record.clone(),
            },
            None => RoutingEvent::InstanceAdded(record.clone()),
        };
        Self::broadcast_routing_batch(
            &mut inner,
            format!("memory:instance:{}", record.instance_id),
            Some("instance.status".to_string()),
            vec![event],
        );
        Ok(())
    }

    async fn remove_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        let mut inner = self.lock_inner();
        if let Some(record) = Self::remove_instance_status_inner(&mut inner, instance_id) {
            Self::broadcast_routing_batch(
                &mut inner,
                format!("memory:instance:remove:{instance_id}"),
                Some("instance.remove".to_string()),
                vec![RoutingEvent::InstanceRemoved(record)],
            );
        }
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

    fn record_service_revision_inner(
        inner: &mut StoreInner,
        record: &ServiceRevisionRecord,
    ) -> Option<ServiceRevisionRecord> {
        let key = (
            record.namespace.clone(),
            record.service.clone(),
            record.revision_hash.clone(),
        );
        inner.service_revisions.insert(key, record.clone())
    }

    fn record_instance_status_inner(
        inner: &mut StoreInner,
        record: &InstanceStatusRecord,
    ) -> Option<InstanceStatusRecord> {
        inner
            .instance_status
            .insert(record.instance_id.clone(), record.clone())
    }

    fn remove_instance_status_inner(
        inner: &mut StoreInner,
        instance_id: &InstanceId,
    ) -> Option<InstanceStatusRecord> {
        inner.instance_status.remove(instance_id)
    }

    fn commit_deploy_inner(inner: &mut StoreInner, command: &DeployCommit) -> Vec<RoutingEvent> {
        let mut events = Vec::new();
        for revision in &command.revisions {
            let old = Self::record_service_revision_inner(inner, revision);
            if let Some(event) = revision_event(old, revision) {
                events.push(event);
            }
        }

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

        let mut removed = inner
            .service_releases
            .extract_if(|(current_namespace, service), _| {
                current_namespace == &command.namespace
                    && touched_services.contains(service.as_str())
            })
            .map(|(_, record)| record)
            .collect::<Vec<_>>();

        for release in &command.releases {
            let old = removed
                .iter()
                .position(|record| {
                    record.namespace == release.namespace && record.service == release.service
                })
                .map(|idx| removed.swap_remove(idx));
            inner.service_releases.insert(
                (release.namespace.clone(), release.service.clone()),
                release.clone(),
            );
            match old {
                Some(old) => events.push(RoutingEvent::ReleaseUpdated {
                    old,
                    new: release.clone(),
                }),
                None => events.push(RoutingEvent::ReleaseAdded(release.clone())),
            }
        }

        for record in removed {
            events.push(RoutingEvent::ReleaseRemoved(record));
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
        events
    }
}

fn revision_event(
    old: Option<ServiceRevisionRecord>,
    new: &ServiceRevisionRecord,
) -> Option<RoutingEvent> {
    match old {
        Some(old) if old == *new => None,
        Some(old) => Some(RoutingEvent::RevisionUpdated {
            old,
            new: new.clone(),
        }),
        None => Some(RoutingEvent::RevisionAdded(new.clone())),
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

    async fn upsert_acme_challenge_readiness(
        &self,
        record: &AcmeChallengeReadinessRecord,
    ) -> Result<()> {
        let mut inner = self.lock_inner();
        inner.acme_challenge_readiness.insert(
            (
                record.hostname.clone(),
                record.token.clone(),
                record.machine_id.clone(),
            ),
            record.clone(),
        );
        Ok(())
    }

    async fn list_acme_challenge_readiness(
        &self,
        hostname: &str,
        token: &str,
    ) -> Result<Vec<AcmeChallengeReadinessRecord>> {
        let inner = self.lock_inner();
        Ok(inner
            .acme_challenge_readiness
            .values()
            .filter(|record| {
                record
                    .hostname
                    .trim_end_matches('.')
                    .eq_ignore_ascii_case(hostname.trim_end_matches('.'))
                    && record.token == token
            })
            .cloned()
            .collect())
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
        let removed_revisions = inner
            .service_revisions
            .drain()
            .map(|(_, record)| record)
            .collect::<Vec<_>>();
        let removed_releases = inner
            .service_releases
            .drain()
            .map(|(_, record)| record)
            .collect::<Vec<_>>();
        let removed_instances = inner
            .instance_status
            .drain()
            .map(|(_, record)| record)
            .collect::<Vec<_>>();
        inner.invites.clear();
        inner.volumes.clear();
        inner.deploys.clear();
        inner.acme_accounts.clear();
        inner.certificates.clear();
        inner.acme_challenges.clear();
        inner.acme_challenge_readiness.clear();

        let mut events = Vec::new();
        for record in removed {
            Self::broadcast_machine(&mut inner, MachineEvent::Removed(record.clone()));
            events.push(RoutingEvent::MachineRemoved(record));
        }
        for record in removed_revisions {
            events.push(RoutingEvent::RevisionRemoved(record));
        }
        for record in removed_releases {
            events.push(RoutingEvent::ReleaseRemoved(record));
        }
        for record in removed_instances {
            events.push(RoutingEvent::InstanceRemoved(record));
        }
        Self::broadcast_routing_batch(
            &mut inner,
            "memory:wipe",
            Some("store.wipe".to_string()),
            events,
        );
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

    fn test_machine(id: impl Into<String>) -> MachineMembership {
        MachineMembership {
            id: MachineId(id.into()),
            public_key: ployz_types::model::PublicKey([0; 32]),
            overlay_ip: ployz_types::model::OverlayIp("fd00::1".parse().expect("valid overlay")),
            topology: ployz_types::model::MachineTopology::local(),
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle: ployz_types::model::MachineLifecycle::Active,
            storage: true,
            storage_participation: ployz_types::model::StorageParticipation::default_authority(),
            created_at: 1,
            updated_at: 1,
            labels: std::collections::BTreeMap::new(),
        }
    }

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
    async fn record_service_revision_updates_routing_snapshot_and_emits_event() {
        let store = MemoryStore::new();
        let (_state, mut event_rx) = store.subscribe_routing_events().await.expect("subscribe");

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

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .expect("refresh event deadline");
        assert!(matches!(
            event,
            Some(RoutingEvent::RevisionAdded(ServiceRevisionRecord { .. }))
        ));

        let snapshot = store
            .load_routing_state()
            .await
            .expect("load routing state");
        assert_eq!(snapshot.revisions.len(), 1);
    }

    #[tokio::test]
    async fn commit_deploy_records_inlined_revisions() {
        let store = MemoryStore::new();
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
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: vec![revision],
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                releases: vec![test_release(&namespace, "api", "rev-1", "deploy-1")],
                volumes: Vec::new(),
                deploy: test_deploy(&namespace, "deploy-1"),
            })
            .await
            .expect("commit deploy");

        let snapshot = store
            .load_routing_state()
            .await
            .expect("load routing state");
        assert_eq!(snapshot.revisions.len(), 1);
        assert_eq!(snapshot.revisions[0].revision_hash, "rev-1");
    }

    #[tokio::test]
    async fn commit_deploy_replaces_touched_releases_and_records_deploy() {
        let store = MemoryStore::new();
        let namespace = Namespace("prod".into());
        let untouched = test_release(&namespace, "worker", "rev-old", "deploy-old");
        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: Vec::new(),
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
                revisions: Vec::new(),
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
    async fn redeploying_existing_service_emits_release_updated() {
        let store = MemoryStore::new();
        let namespace = Namespace("prod".into());

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: Vec::new(),
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                releases: vec![test_release(&namespace, "api", "rev-old", "deploy-old")],
                volumes: Vec::new(),
                deploy: test_deploy(&namespace, "deploy-old"),
            })
            .await
            .expect("seed deploy");

        let (_state, mut event_rx) = store.subscribe_routing_events().await.expect("subscribe");

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: Vec::new(),
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                releases: vec![test_release(&namespace, "api", "rev-new", "deploy-new")],
                volumes: Vec::new(),
                deploy: test_deploy(&namespace, "deploy-new"),
            })
            .await
            .expect("redeploy");

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .expect("await redeploy event")
            .expect("event present");

        match event {
            RoutingEvent::ReleaseUpdated { old, new } => {
                assert_eq!(old.release.primary_revision_hash, "rev-old");
                assert_eq!(new.release.primary_revision_hash, "rev-new");
            }
            other => panic!(
                "redeploy of an existing (namespace, service) must emit ReleaseUpdated, got {other:?}"
            ),
        }
    }

    #[tokio::test]
    async fn commit_deploy_removed_service_emits_release_removed() {
        let store = MemoryStore::new();
        let namespace = Namespace("prod".into());
        let removed = test_release(&namespace, "worker", "rev-old", "deploy-old");

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: Vec::new(),
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                releases: vec![
                    test_release(&namespace, "api", "rev-old", "deploy-old"),
                    removed.clone(),
                ],
                volumes: Vec::new(),
                deploy: test_deploy(&namespace, "deploy-old"),
            })
            .await
            .expect("seed deploy");
        let (_state, mut event_rx) = store.subscribe_routing_events().await.expect("subscribe");

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: Vec::new(),
                removed_services: vec!["worker".into()],
                removed_volumes: Vec::new(),
                releases: Vec::new(),
                volumes: Vec::new(),
                deploy: test_deploy(&namespace, "deploy-new"),
            })
            .await
            .expect("remove service");

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .expect("await removal event")
            .expect("event present");
        assert_eq!(event, RoutingEvent::ReleaseRemoved(removed));
    }

    #[tokio::test]
    async fn commit_deploy_removed_volumes_are_scoped_to_namespace() {
        let store = MemoryStore::new();
        let prod = Namespace("prod".into());
        let staging = Namespace("staging".into());
        let deploy_id = DeployId("deploy-1".into());

        store
            .commit_deploy(&DeployCommit {
                namespace: prod.clone(),
                revisions: Vec::new(),
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                releases: Vec::new(),
                volumes: vec![test_volume(&prod, "data", &deploy_id)],
                deploy: test_deploy(&prod, "deploy-prod"),
            })
            .await
            .expect("seed prod volume");
        store
            .commit_deploy(&DeployCommit {
                namespace: staging.clone(),
                revisions: Vec::new(),
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                releases: Vec::new(),
                volumes: vec![test_volume(&staging, "data", &deploy_id)],
                deploy: test_deploy(&staging, "deploy-staging"),
            })
            .await
            .expect("seed staging volume");

        store
            .commit_deploy(&DeployCommit {
                namespace: prod.clone(),
                revisions: Vec::new(),
                removed_services: Vec::new(),
                removed_volumes: vec!["data".into()],
                releases: Vec::new(),
                volumes: Vec::new(),
                deploy: test_deploy(&prod, "deploy-prod-remove"),
            })
            .await
            .expect("remove prod volume");

        assert!(
            store
                .get_volume(&prod, "data")
                .await
                .expect("load prod volume")
                .is_none()
        );
        assert!(
            store
                .get_volume(&staging, "data")
                .await
                .expect("load staging volume")
                .is_some()
        );
    }

    #[tokio::test]
    async fn instance_status_writes_update_routing_snapshot_and_emit_events() {
        let store = MemoryStore::new();
        let namespace = Namespace("prod".into());
        let status = test_instance_status(&namespace, "inst-1");
        let (_state, mut event_rx) = store.subscribe_routing_events().await.expect("subscribe");

        store
            .record_instance_status(&status)
            .await
            .expect("record instance status");
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .expect("record refresh deadline");
        assert!(matches!(
            event,
            Some(RoutingEvent::InstanceAdded(InstanceStatusRecord { .. }))
        ));

        store
            .remove_instance_status(&status.instance_id)
            .await
            .expect("remove instance status");
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .expect("remove refresh deadline");
        assert!(matches!(
            event,
            Some(RoutingEvent::InstanceRemoved(InstanceStatusRecord { .. }))
        ));

        let snapshot = store
            .load_routing_state()
            .await
            .expect("load routing state");
        assert!(snapshot.instances.is_empty());
    }

    #[tokio::test]
    async fn acme_readiness_is_scoped_by_hostname_token_and_machine() {
        let store = MemoryStore::new();
        store
            .upsert_acme_challenge_readiness(&AcmeChallengeReadinessRecord {
                hostname: "example.com".into(),
                token: "old-token".into(),
                machine_id: MachineId("machine-a".into()),
                observed_at: 1,
            })
            .await
            .expect("write old readiness");
        store
            .upsert_acme_challenge_readiness(&AcmeChallengeReadinessRecord {
                hostname: "example.com".into(),
                token: "new-token".into(),
                machine_id: MachineId("machine-a".into()),
                observed_at: 2,
            })
            .await
            .expect("write new readiness");

        let records = store
            .list_acme_challenge_readiness("example.com", "new-token")
            .await
            .expect("list readiness");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].token, "new-token");
        assert_eq!(records[0].machine_id, MachineId("machine-a".into()));
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

    fn test_volume(namespace: &Namespace, volume_name: &str, deploy_id: &DeployId) -> VolumeRecord {
        VolumeRecord {
            namespace: namespace.clone(),
            volume_name: volume_name.into(),
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
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle: ployz_types::model::MachineLifecycle::Active,
            storage: true,
            storage_participation: ployz_types::model::StorageParticipation::default_authority(),
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
    async fn machine_changes_trigger_routing_events() {
        let store = MemoryStore::new();
        let (_state, mut event_rx) = store.subscribe_routing_events().await.expect("subscribe");
        let machine = MachineMembership {
            id: MachineId("machine-1".into()),
            public_key: ployz_types::model::PublicKey([0; 32]),
            overlay_ip: ployz_types::model::OverlayIp("fd00::1".parse().expect("valid overlay")),
            topology: ployz_types::model::MachineTopology::local(),
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle: ployz_types::model::MachineLifecycle::Active,
            storage: true,
            storage_participation: ployz_types::model::StorageParticipation::default_authority(),
            created_at: 1,
            updated_at: 1,
            labels: std::collections::BTreeMap::new(),
        };

        store
            .upsert_self_machine(&machine)
            .await
            .expect("upsert machine");

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .expect("refresh event deadline");
        assert!(matches!(
            event,
            Some(RoutingEvent::MachineAdded(MachineMembership { .. }))
        ));
    }

    #[tokio::test]
    async fn routing_batch_subscription_returns_snapshot_then_metadata_rich_batches() {
        let store = MemoryStore::new();
        let machine = test_machine("machine-1");
        store
            .upsert_self_machine(&machine)
            .await
            .expect("seed machine");

        let (state, mut batches) = store
            .subscribe_routing_batches(crate::RoutingSubscription::durable("test.consumer"))
            .await
            .expect("subscribe routing batches");

        assert_eq!(state.machines, vec![machine.clone()]);

        let mut updated = machine.clone();
        updated.labels.insert("role".into(), "gateway".into());
        store
            .upsert_self_machine(&updated)
            .await
            .expect("update machine");

        let batch = tokio::time::timeout(std::time::Duration::from_secs(1), batches.recv())
            .await
            .expect("routing batch deadline")
            .expect("routing batch")
            .expect("routing batch should be successful");

        assert_eq!(batch.batch_id, "memory:machine:machine-1");
        assert_eq!(batch.cause.as_deref(), Some("machine.upsert"));
        let [event] = batch.events.as_slice() else {
            panic!("expected one routing event, got {:?}", batch.events);
        };
        let RoutingEvent::MachineUpdated { old, new } = event else {
            panic!("expected machine update event, got {event:?}");
        };
        assert_eq!(old.labels.get("role"), None);
        assert_eq!(new.labels.get("role").map(String::as_str), Some("gateway"));

        batch.ack().await.expect("memory routing ack is a no-op");
    }

    #[tokio::test]
    async fn full_routing_event_channel_closes_subscriber() {
        let store = MemoryStore::new();
        let (_state, mut event_rx) = store.subscribe_routing_events().await.expect("subscribe");

        for index in 0..1030 {
            let revision = ServiceRevisionRecord {
                namespace: Namespace("prod".into()),
                service: format!("api-{index}"),
                revision_hash: "rev-1".into(),
                spec_json: "{}".into(),
                created_by: MachineId("machine-1".into()),
                created_at: 1,
            };
            store
                .record_service_revision(&DeployRevisionUpsert { revision })
                .await
                .expect("record revision");
        }

        let mut received = 0;
        while event_rx.recv().await.is_some() {
            received += 1;
        }
        assert_eq!(
            received, 1024,
            "full delta channels must close after buffered events instead of silently skipping one"
        );
    }

    #[tokio::test]
    async fn full_machine_event_channel_closes_subscriber() {
        let store = MemoryStore::new();
        let (_snapshot, mut event_rx) = store.subscribe_machines().await.expect("subscribe");

        for index in 0..70 {
            store
                .upsert_self_machine(&test_machine(format!("machine-{index}")))
                .await
                .expect("upsert machine");
        }

        let mut received = 0;
        while event_rx.recv().await.is_some() {
            received += 1;
        }
        assert_eq!(
            received, 64,
            "full machine event channels must close after buffered events instead of silently skipping one"
        );
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
                revisions: Vec::new(),
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
