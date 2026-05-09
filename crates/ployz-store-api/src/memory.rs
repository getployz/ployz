use crate::deploy_commit_facts::DeployCommitFacts;
use crate::{
    AcmeChallengeSubscription, CertificateStore, CertificateSubscription, DeployCommit,
    DeployStore, InstanceStatusStore, InviteStore, MachineMembershipStore, RoutingEventEnvelope,
    RoutingEventSubscription, RoutingStateStore, StoreRuntimeControl, SyncProbe, SyncStatus,
};
use async_trait::async_trait;
use ployz_types::error::{Error, Result, SubscriptionStream};
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeEvent, AcmeChallengeReadinessRecord, AcmeChallengeRecord,
    CertificateEvent, CertificateRecord, DeployId, DeployPhaseId, DeployPhaseRecord, DeployRecord,
    InstanceId, InstanceStatusRecord, InviteRecord, MachineEvent, MachineId, MachineMembership,
    RoutingEvent, RoutingState, ServiceBranchLineageRecord, ServiceReleaseRecord,
    ServiceRevisionRecord, VolumeRecord,
};
use ployz_types::spec::Namespace;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use tokio::sync::{broadcast, mpsc};
use tracing::warn;

const MEMORY_SUBSCRIPTION_CAPACITY: usize = 1024;

pub struct MemoryStore {
    inner: Mutex<StoreInner>,
}

struct StoreInner {
    machines: BTreeMap<MachineId, MachineMembership>,
    machine_events: broadcast::Sender<MachineEvent>,
    routing_events: broadcast::Sender<MemoryRoutingEvent>,
    invites: BTreeMap<String, InviteRecord>,
    deploy_commit_facts: DeployCommitFacts,
    deploy_records: HashMap<DeployId, DeployRecord>,
    deploy_phase_records: HashMap<(Namespace, DeployId, DeployPhaseId), DeployPhaseRecord>,
    instance_status: HashMap<InstanceId, InstanceStatusRecord>,
    acme_accounts: HashMap<String, AcmeAccountRecord>,
    certificates: BTreeMap<String, CertificateRecord>,
    certificate_events: broadcast::Sender<CertificateEvent>,
    acme_challenges: BTreeMap<(String, String), AcmeChallengeRecord>,
    acme_challenge_events: broadcast::Sender<AcmeChallengeEvent>,
    acme_challenge_readiness: BTreeMap<(String, String, MachineId), AcmeChallengeReadinessRecord>,
    sync_status: SyncStatus,
}

#[derive(Debug, Clone)]
struct MemoryRoutingEvent {
    event_id: String,
    cause: Option<String>,
    event: RoutingEvent,
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
                machines: BTreeMap::new(),
                machine_events: broadcast::channel(MEMORY_SUBSCRIPTION_CAPACITY).0,
                routing_events: broadcast::channel(MEMORY_SUBSCRIPTION_CAPACITY).0,
                invites: BTreeMap::new(),
                deploy_commit_facts: DeployCommitFacts::new(),
                deploy_records: HashMap::new(),
                deploy_phase_records: HashMap::new(),
                instance_status: HashMap::new(),
                acme_accounts: HashMap::new(),
                certificates: BTreeMap::new(),
                certificate_events: broadcast::channel(MEMORY_SUBSCRIPTION_CAPACITY).0,
                acme_challenges: BTreeMap::new(),
                acme_challenge_events: broadcast::channel(MEMORY_SUBSCRIPTION_CAPACITY).0,
                acme_challenge_readiness: BTreeMap::new(),
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
        let _ = inner.machine_events.send(event);
    }

    fn broadcast_routing_events(
        inner: &mut StoreInner,
        operation_id: impl Into<String>,
        cause: Option<String>,
        events: Vec<RoutingEvent>,
    ) {
        let operation_id = operation_id.into();
        for (index, event) in events.into_iter().enumerate() {
            Self::broadcast_routing_event(
                inner,
                format!("{operation_id}:{}", index + 1),
                cause.clone(),
                event,
            );
        }
    }

    fn broadcast_routing_event(
        inner: &mut StoreInner,
        event_id: impl Into<String> + Clone,
        cause: Option<String>,
        event: RoutingEvent,
    ) {
        let event_id = event_id.into();
        let _ = inner.routing_events.send(MemoryRoutingEvent {
            event_id,
            cause,
            event,
        });
    }

    fn routing_state(inner: &StoreInner) -> RoutingState {
        let mut state = RoutingState {
            machines: inner.machines.values().cloned().collect(),
            revisions: inner.deploy_commit_facts.all_revisions(),
            releases: inner.deploy_commit_facts.all_releases(),
            instances: inner.instance_status.values().cloned().collect(),
        };
        sort_instance_status(&mut state.instances);
        state
    }

    pub async fn load_routing_state(&self) -> Result<RoutingState> {
        let inner = self.lock_inner();
        Ok(Self::routing_state(&inner))
    }

    pub async fn subscribe_routing_events(
        &self,
    ) -> Result<(RoutingState, mpsc::Receiver<RoutingEvent>)> {
        let (state, mut envelopes) = self.subscribe_routing_events_internal().await?;
        let (tx, rx) = mpsc::channel(1024);
        tokio::spawn(async move {
            while let Some(envelope) = envelopes.recv().await {
                let Ok(envelope) = envelope else {
                    return;
                };
                if tx.send(envelope.event).await.is_err() {
                    return;
                }
            }
        });
        Ok((state, rx))
    }

    fn broadcast_certificate(inner: &mut StoreInner, event: CertificateEvent) {
        let _ = inner.certificate_events.send(event);
    }

    fn broadcast_acme_challenge(inner: &mut StoreInner, event: AcmeChallengeEvent) {
        let _ = inner.acme_challenge_events.send(event);
    }
}

fn subscribe_broadcast<T>(
    stream: SubscriptionStream,
    mut events: broadcast::Receiver<T>,
) -> mpsc::Receiver<Result<T>>
where
    T: Clone + Send + 'static,
{
    let (tx, rx) = mpsc::channel(MEMORY_SUBSCRIPTION_CAPACITY);
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if tx.send(Ok(event)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(%stream, skipped, "memory subscriber lagged, closing event stream");
                    let _ = tx
                        .send(Err(Error::subscription_lagged(stream, skipped)))
                        .await;
                    break;
                }
            }
        }
    });
    rx
}

fn subscribe_routing_broadcast(
    mut events: broadcast::Receiver<MemoryRoutingEvent>,
) -> mpsc::Receiver<crate::RoutingEventSubscriptionUpdate> {
    let (tx, rx) = mpsc::channel(MEMORY_SUBSCRIPTION_CAPACITY);
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let envelope =
                        RoutingEventEnvelope::unacked(event.event_id, event.cause, event.event);
                    if tx.send(Ok(envelope)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(
                        skipped,
                        "memory routing subscriber lagged, closing event stream"
                    );
                    let _ = tx
                        .send(Err(Error::subscription_lagged(
                            SubscriptionStream::Routing,
                            skipped,
                        )))
                        .await;
                    break;
                }
            }
        }
    });
    rx
}

#[async_trait]
impl SyncProbe for MemoryStore {
    async fn sync_status(&self) -> Result<SyncStatus> {
        Ok(self.lock_inner().sync_status)
    }
}

#[async_trait]
impl crate::PeerRttStore for MemoryStore {}

#[async_trait]
impl MachineMembershipStore for MemoryStore {
    async fn list_machines(&self) -> Result<Vec<MachineMembership>> {
        let inner = self.lock_inner();
        Ok(inner.machines.values().cloned().collect())
    }

    async fn upsert_self_machine(&self, record: &MachineMembership) -> Result<()> {
        let mut inner = self.lock_inner();
        inner.machines.insert(record.id.clone(), record.clone());
        Self::broadcast_machine(&mut inner, MachineEvent::Upsert(record.clone()));
        Self::broadcast_routing_events(
            &mut inner,
            format!("memory:machine:{}", record.id),
            Some("machine.upsert".to_string()),
            vec![RoutingEvent::MachineUpsert(record.clone())],
        );
        Ok(())
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        let mut inner = self.lock_inner();
        if let Some(record) = inner.machines.remove(id) {
            Self::broadcast_machine(
                &mut inner,
                MachineEvent::Removed {
                    id: record.id.clone(),
                },
            );
            Self::broadcast_routing_events(
                &mut inner,
                format!("memory:machine:delete:{id}"),
                Some("machine.delete".to_string()),
                vec![RoutingEvent::MachineRemoved { id: record.id }],
            );
        }
        Ok(())
    }

    async fn subscribe_machines(&self) -> Result<crate::MachineSubscription> {
        let inner = self.lock_inner();
        let snapshot = inner.machines.values().cloned().collect::<Vec<_>>();
        let receiver = subscribe_broadcast(
            SubscriptionStream::Machine,
            inner.machine_events.subscribe(),
        );
        Ok((snapshot, receiver))
    }
}

#[async_trait]
impl RoutingStateStore for MemoryStore {
    async fn load_routing_state(&self) -> Result<RoutingState> {
        self.load_routing_state().await
    }

    async fn subscribe_routing_events(&self) -> Result<RoutingEventSubscription> {
        self.subscribe_routing_events_internal().await
    }
}

impl MemoryStore {
    async fn subscribe_routing_events_internal(&self) -> Result<RoutingEventSubscription> {
        let inner = self.lock_inner();
        let state = Self::routing_state(&inner);
        let receiver = subscribe_routing_broadcast(inner.routing_events.subscribe());
        Ok((state, receiver))
    }
}

#[async_trait]
impl InviteStore for MemoryStore {
    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        let mut inner = self.lock_inner();
        if inner.invites.contains_key(&invite.invite_id) {
            return Err(Error::invite_already_exists(invite.invite_id.clone()));
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
        Ok(inner.invites.values().cloned().collect())
    }

    async fn redeem_invite(
        &self,
        invite_id: &str,
        machine_id: &MachineId,
        now_unix_secs: u64,
    ) -> Result<InviteRecord> {
        let mut inner = self.lock_inner();
        let Some(invite) = inner.invites.get(invite_id).cloned() else {
            return Err(Error::invite_not_found(invite_id));
        };

        if invite.revoked_at.is_some() {
            return Err(Error::invite_revoked(invite_id));
        }

        if now_unix_secs > invite.expires_at {
            return Err(Error::invite_expired(invite_id));
        }

        if let Some(consumed_by) = &invite.consumed_by {
            if consumed_by == machine_id {
                return Ok(invite);
            }
            return Err(Error::invite_consumed(invite_id));
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
        let invite = inner
            .invites
            .get(invite_id)
            .ok_or_else(|| Error::invite_not_found(invite_id))?;
        if invite.consumed_by.is_some() {
            return Err(Error::invite_consumed(invite_id));
        }

        let mut next_invite = invite.clone();
        next_invite.revoked_at = Some(now_unix_secs);
        inner
            .invites
            .insert(invite_id.to_string(), next_invite.clone());
        Ok(next_invite)
    }
}

#[async_trait]
impl DeployStore for MemoryStore {
    async fn list_deploy_revisions(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceRevisionRecord>> {
        let inner = self.lock_inner();
        Ok(inner.deploy_commit_facts.revisions(namespace))
    }

    async fn list_deploy_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        let inner = self.lock_inner();
        Ok(inner.deploy_commit_facts.releases(namespace))
    }

    async fn list_volumes(&self, namespace: &Namespace) -> Result<Vec<VolumeRecord>> {
        let inner = self.lock_inner();
        Ok(inner.deploy_commit_facts.volumes(namespace))
    }

    async fn list_service_branch_lineage(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceBranchLineageRecord>> {
        let inner = self.lock_inner();
        Ok(inner.deploy_commit_facts.service_branch_lineage(namespace))
    }

    async fn get_volume(
        &self,
        namespace: &Namespace,
        volume_name: &str,
    ) -> Result<Option<VolumeRecord>> {
        let inner = self.lock_inner();
        Ok(inner
            .deploy_commit_facts
            .volume(namespace, volume_name)
            .cloned())
    }

    async fn commit_deploy(&self, command: &DeployCommit) -> Result<()> {
        let mut inner = self.lock_inner();
        let events = inner.deploy_commit_facts.apply_commit_events(command);
        Self::broadcast_routing_events(
            &mut inner,
            format!("memory:deploy:{}", command.deploy.deploy_id),
            Some("deploy.commit".to_string()),
            events,
        );
        Ok(())
    }

    async fn write_deploy_status(&self, deploy: &DeployRecord) -> Result<()> {
        let mut inner = self.lock_inner();
        inner
            .deploy_records
            .insert(deploy.deploy_id.clone(), deploy.clone());
        Ok(())
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        let inner = self.lock_inner();
        Ok(inner.deploy_records.get(deploy_id).cloned())
    }

    async fn upsert_deploy_phase(&self, phase: &DeployPhaseRecord) -> Result<()> {
        let mut inner = self.lock_inner();
        inner.deploy_phase_records.insert(
            (
                phase.namespace.clone(),
                phase.deploy_id.clone(),
                phase.phase_id.clone(),
            ),
            phase.clone(),
        );
        Ok(())
    }

    async fn get_deploy_phase(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
        phase_id: &DeployPhaseId,
    ) -> Result<Option<DeployPhaseRecord>> {
        let inner = self.lock_inner();
        Ok(inner
            .deploy_phase_records
            .get(&(namespace.clone(), deploy_id.clone(), phase_id.clone()))
            .cloned())
    }

    async fn list_deploy_phases(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
    ) -> Result<Vec<DeployPhaseRecord>> {
        let inner = self.lock_inner();
        let mut phases = inner
            .deploy_phase_records
            .values()
            .filter(|phase| phase.namespace == *namespace && phase.deploy_id == *deploy_id)
            .cloned()
            .collect::<Vec<_>>();
        sort_deploy_phases(&mut phases);
        Ok(phases)
    }
}

fn sort_deploy_phases(phases: &mut [DeployPhaseRecord]) {
    phases.sort_by(|left, right| {
        (
            left.namespace.0.as_str(),
            left.deploy_id.0.as_str(),
            left.order,
            left.phase_id.0.as_str(),
        )
            .cmp(&(
                right.namespace.0.as_str(),
                right.deploy_id.0.as_str(),
                right.order,
                right.phase_id.0.as_str(),
            ))
    });
}

#[async_trait]
impl InstanceStatusStore for MemoryStore {
    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        let inner = self.lock_inner();
        let mut records = inner
            .instance_status
            .values()
            .filter(|record| record.namespace == *namespace)
            .cloned()
            .collect::<Vec<_>>();
        sort_instance_status(&mut records);
        Ok(records)
    }

    async fn record_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        let mut inner = self.lock_inner();
        Self::record_instance_status_inner(&mut inner, record);
        Self::broadcast_routing_events(
            &mut inner,
            format!("memory:instance:{}", record.instance_id),
            Some("instance.status".to_string()),
            vec![RoutingEvent::InstanceUpsert(record.clone())],
        );
        Ok(())
    }

    async fn remove_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        let mut inner = self.lock_inner();
        if let Some(record) = Self::remove_instance_status_inner(&mut inner, instance_id) {
            Self::broadcast_routing_events(
                &mut inner,
                format!("memory:instance:remove:{instance_id}"),
                Some("instance.remove".to_string()),
                vec![RoutingEvent::InstanceRemoved {
                    instance_id: record.instance_id,
                }],
            );
        }
        Ok(())
    }
}

impl MemoryStore {
    fn record_instance_status_inner(inner: &mut StoreInner, record: &InstanceStatusRecord) {
        inner
            .instance_status
            .insert(record.instance_id.clone(), record.clone());
    }

    fn remove_instance_status_inner(
        inner: &mut StoreInner,
        instance_id: &InstanceId,
    ) -> Option<InstanceStatusRecord> {
        inner.instance_status.remove(instance_id)
    }
}

fn sort_instance_status(instances: &mut [InstanceStatusRecord]) {
    instances.sort_by(|left, right| {
        (left.namespace.clone(), left.instance_id.0.as_str())
            .cmp(&(right.namespace.clone(), right.instance_id.0.as_str()))
    });
}

#[async_trait]
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
        inner
            .certificates
            .insert(record.hostname.clone(), record.clone());
        Self::broadcast_certificate(&mut inner, CertificateEvent::Upsert(record.clone()));
        Ok(())
    }

    async fn list_acme_challenges(&self) -> Result<Vec<AcmeChallengeRecord>> {
        let inner = self.lock_inner();
        Ok(inner.acme_challenges.values().cloned().collect())
    }

    async fn upsert_acme_challenge(&self, record: &AcmeChallengeRecord) -> Result<()> {
        let mut inner = self.lock_inner();
        let key = (record.hostname.clone(), record.token.clone());
        inner.acme_challenges.insert(key, record.clone());
        Self::broadcast_acme_challenge(&mut inner, AcmeChallengeEvent::Upsert(record.clone()));
        Ok(())
    }

    async fn delete_acme_challenge(&self, hostname: &str, token: &str) -> Result<()> {
        let mut inner = self.lock_inner();
        if let Some(record) = inner
            .acme_challenges
            .remove(&(hostname.to_string(), token.to_string()))
        {
            Self::broadcast_acme_challenge(
                &mut inner,
                AcmeChallengeEvent::Removed {
                    hostname: record.hostname,
                    token: record.token,
                },
            );
        }
        Ok(())
    }

    async fn subscribe_certificates(&self) -> Result<CertificateSubscription> {
        let inner = self.lock_inner();
        let snapshot = inner.certificates.values().cloned().collect::<Vec<_>>();
        let receiver = subscribe_broadcast(
            SubscriptionStream::Certificate,
            inner.certificate_events.subscribe(),
        );
        Ok((snapshot, receiver))
    }

    async fn subscribe_acme_challenges(&self) -> Result<AcmeChallengeSubscription> {
        let inner = self.lock_inner();
        let snapshot = inner.acme_challenges.values().cloned().collect::<Vec<_>>();
        let receiver = subscribe_broadcast(
            SubscriptionStream::AcmeChallenge,
            inner.acme_challenge_events.subscribe(),
        );
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
        let records = inner
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
            .collect::<Vec<_>>();
        Ok(records)
    }
}

impl MemoryStore {
    pub async fn wipe_data(&self) -> Result<()> {
        let mut inner = self.lock_inner();
        let removed = std::mem::take(&mut inner.machines)
            .into_values()
            .collect::<Vec<_>>();
        let removed_deploy_commit_facts = std::mem::take(&mut inner.deploy_commit_facts);
        let removed_revisions = removed_deploy_commit_facts.all_revisions();
        let removed_releases = removed_deploy_commit_facts.all_releases();
        let removed_instances = inner
            .instance_status
            .drain()
            .map(|(_, record)| record)
            .collect::<Vec<_>>();
        inner.deploy_phase_records.clear();
        inner.invites.clear();
        inner.acme_accounts.clear();
        inner.certificates.clear();
        inner.acme_challenges.clear();
        inner.acme_challenge_readiness.clear();

        let mut events = Vec::new();
        for record in removed {
            Self::broadcast_machine(
                &mut inner,
                MachineEvent::Removed {
                    id: record.id.clone(),
                },
            );
            events.push(RoutingEvent::MachineRemoved { id: record.id });
        }
        for record in removed_revisions {
            events.push(RoutingEvent::RevisionRemoved {
                namespace: record.namespace,
                service: record.service,
                revision_hash: record.revision_hash,
            });
        }
        for record in removed_releases {
            events.push(RoutingEvent::ReleaseRemoved {
                namespace: record.namespace,
                service: record.service,
            });
        }
        for record in removed_instances {
            events.push(RoutingEvent::InstanceRemoved {
                instance_id: record.instance_id,
            });
        }
        Self::broadcast_routing_events(
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
    use ployz_types::model::{
        DeployPhaseCommitPolicy, DeployPhaseFailure, DeployPhaseRollbackPolicy, DeployPhaseState,
        ServiceBranchLineageRecord, ServiceRevisionRecord,
    };

    fn test_machine(id: impl Into<String>) -> MachineMembership {
        MachineMembership {
            id: MachineId(id.into()),
            public_key: ployz_types::model::PublicKey([0; 32]),
            overlay_ip: ployz_types::model::OverlayIp("fd00::1".parse().expect("valid overlay")),
            topology: ployz_types::model::MachineTopology::local(),
            region_role: ployz_types::model::RegionRole::HomeData,
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
            Err(Error::InviteConsumed { invite_id }) if invite_id == "inv-1"
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
            Err(Error::InviteExpired { invite_id }) if invite_id == "inv-2"
        ));
    }

    #[tokio::test]
    async fn deploy_phase_records_roundtrip_and_list_in_contract_order() {
        let store = MemoryStore::new();
        let namespace = Namespace("prod".into());
        let deploy_id = DeployId("deploy-1".into());
        let later = test_deploy_phase(&namespace, &deploy_id, "web", 1);
        let earlier = test_deploy_phase(&namespace, &deploy_id, "db", 0);

        store
            .upsert_deploy_phase(&later)
            .await
            .expect("write later phase");
        store
            .upsert_deploy_phase(&earlier)
            .await
            .expect("write earlier phase");

        assert_eq!(
            store
                .get_deploy_phase(
                    &namespace,
                    &deploy_id,
                    &ployz_types::model::DeployPhaseId("db".into())
                )
                .await
                .expect("read deploy phase"),
            Some(earlier.clone())
        );
        assert_eq!(
            store
                .list_deploy_phases(&namespace, &deploy_id)
                .await
                .expect("list deploy phases"),
            vec![earlier, later]
        );
    }

    #[tokio::test]
    async fn deploy_phase_upsert_replaces_state_for_same_identity() {
        let store = MemoryStore::new();
        let namespace = Namespace("prod".into());
        let deploy_id = DeployId("deploy-1".into());
        let mut phase = test_deploy_phase(&namespace, &deploy_id, "deploy", 0);
        store
            .upsert_deploy_phase(&phase)
            .await
            .expect("write running phase");

        phase.state = DeployPhaseState::Failed {
            completed_at: 20,
            failure: DeployPhaseFailure {
                code: "START_FAILED".into(),
                message: "start failed".into(),
            },
        };
        store
            .upsert_deploy_phase(&phase)
            .await
            .expect("replace phase");

        assert_eq!(
            store
                .list_deploy_phases(&namespace, &deploy_id)
                .await
                .expect("list deploy phases"),
            vec![phase]
        );
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
                branch_lineage: Vec::new(),
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
    async fn commit_deploy_records_service_branch_lineage() {
        let store = MemoryStore::new();
        let namespace = Namespace("pr-39".into());
        let lineage = ServiceBranchLineageRecord {
            namespace: namespace.clone(),
            service: "web".into(),
            revision_hash: "rev-target".into(),
            source_namespace: Namespace("prod".into()),
            source_service: "web".into(),
            source_revision_hash: "rev-source".into(),
            deploy_id: DeployId("deploy-branch".into()),
            created_at: 2,
        };

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: Vec::new(),
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: vec![lineage.clone()],
                releases: Vec::new(),
                volumes: Vec::new(),
                deploy: test_deploy(&namespace, "deploy-branch"),
            })
            .await
            .expect("commit deploy");

        let records = store
            .list_service_branch_lineage(&namespace)
            .await
            .expect("list branch lineage");
        assert_eq!(records, vec![lineage]);
    }

    #[tokio::test]
    async fn commit_deploy_replaces_touched_releases() {
        let store = MemoryStore::new();
        let namespace = Namespace("prod".into());
        let untouched = test_release(&namespace, "worker", "rev-old", "deploy-old");
        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: Vec::new(),
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: Vec::new(),
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
                branch_lineage: Vec::new(),
                releases: vec![test_release(&namespace, "api", "rev-new", "deploy-new")],
                volumes: Vec::new(),
                deploy: test_deploy(&namespace, "deploy-new"),
            })
            .await
            .expect("commit deploy");

        let releases = store
            .list_deploy_releases(&namespace)
            .await
            .expect("list deploy releases");
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].service, "api");
        assert_eq!(releases[0].release.primary_revision_hash, "rev-new");
    }

    #[tokio::test]
    async fn deploy_status_is_written_explicitly() {
        let store = MemoryStore::new();
        let namespace = Namespace("prod".into());
        let deploy = test_deploy(&namespace, "deploy-new");

        assert!(
            store
                .get_deploy(&deploy.deploy_id)
                .await
                .expect("get missing deploy")
                .is_none()
        );

        store
            .write_deploy_status(&deploy)
            .await
            .expect("write deploy status");

        assert_eq!(
            store
                .get_deploy(&deploy.deploy_id)
                .await
                .expect("get deploy"),
            Some(deploy)
        );
    }

    #[tokio::test]
    async fn deploy_reads_return_contract_identity_order() {
        let store = MemoryStore::new();
        let namespace = Namespace("prod".into());

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: vec![
                    test_revision(&namespace, "worker", "rev-b"),
                    test_revision(&namespace, "api", "rev-b"),
                    test_revision(&namespace, "api", "rev-a"),
                ],
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: Vec::new(),
                releases: vec![
                    test_release(&namespace, "worker", "rev-b", "deploy-1"),
                    test_release(&namespace, "api", "rev-a", "deploy-1"),
                ],
                volumes: Vec::new(),
                deploy: test_deploy(&namespace, "deploy-1"),
            })
            .await
            .expect("seed deploy");
        let mut instance_b = test_instance_status(&namespace, "instance-b");
        instance_b.service = "worker".into();
        store
            .record_instance_status(&instance_b)
            .await
            .expect("record instance b");
        store
            .record_instance_status(&test_instance_status(&namespace, "instance-a"))
            .await
            .expect("record instance a");

        let revisions = store
            .list_deploy_revisions(&namespace)
            .await
            .expect("list deploy revisions");
        let releases = store
            .list_deploy_releases(&namespace)
            .await
            .expect("list deploy releases");
        let instances = store
            .list_instance_status(&namespace)
            .await
            .expect("list instance status");

        assert_eq!(
            revisions
                .iter()
                .map(|revision| (revision.service.as_str(), revision.revision_hash.as_str()))
                .collect::<Vec<_>>(),
            [("api", "rev-a"), ("api", "rev-b"), ("worker", "rev-b")]
        );
        assert_eq!(
            releases
                .iter()
                .map(|release| release.service.as_str())
                .collect::<Vec<_>>(),
            ["api", "worker"]
        );
        assert_eq!(
            instances
                .iter()
                .map(|instance| instance.instance_id.0.as_str())
                .collect::<Vec<_>>(),
            ["instance-a", "instance-b"]
        );
    }

    #[tokio::test]
    async fn list_deploy_releases_returns_contract_identity_order() {
        let store = MemoryStore::new();
        let namespace = Namespace("prod".into());

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: Vec::new(),
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: Vec::new(),
                releases: vec![
                    test_release(&namespace, "worker", "rev-b", "deploy-1"),
                    test_release(&namespace, "api", "rev-a", "deploy-1"),
                ],
                volumes: Vec::new(),
                deploy: test_deploy(&namespace, "deploy-1"),
            })
            .await
            .expect("seed releases");

        let releases = store
            .list_deploy_releases(&namespace)
            .await
            .expect("list releases");

        assert_eq!(
            releases
                .iter()
                .map(|release| release.service.as_str())
                .collect::<Vec<_>>(),
            ["api", "worker"]
        );
    }

    #[tokio::test]
    async fn redeploying_existing_service_emits_release_upsert() {
        let store = MemoryStore::new();
        let namespace = Namespace("prod".into());

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: Vec::new(),
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: Vec::new(),
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
                branch_lineage: Vec::new(),
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
            RoutingEvent::ReleaseUpsert(new) => {
                assert_eq!(new.release.primary_revision_hash, "rev-new");
            }
            other => panic!("redeploy must emit release upsert fact, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn multi_event_deploy_broadcast_uses_one_event_id_per_fact() {
        let store = MemoryStore::new();
        let namespace = Namespace("prod".into());
        let (_state, mut envelopes) = store
            .subscribe_routing_events_internal()
            .await
            .expect("subscribe routing events");

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: vec![test_revision(&namespace, "api", "rev-a")],
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: Vec::new(),
                releases: vec![test_release(&namespace, "api", "rev-a", "deploy-new")],
                volumes: Vec::new(),
                deploy: test_deploy(&namespace, "deploy-new"),
            })
            .await
            .expect("deploy");

        let first = tokio::time::timeout(std::time::Duration::from_secs(1), envelopes.recv())
            .await
            .expect("first event deadline")
            .expect("first event")
            .expect("first event should be successful");
        let second = tokio::time::timeout(std::time::Duration::from_secs(1), envelopes.recv())
            .await
            .expect("second event deadline")
            .expect("second event")
            .expect("second event should be successful");

        assert_eq!(first.event_id, "memory:deploy:deploy-new:1");
        assert_eq!(second.event_id, "memory:deploy:deploy-new:2");
        assert!(matches!(first.event, RoutingEvent::RevisionUpsert(_)));
        assert!(matches!(second.event, RoutingEvent::ReleaseUpsert(_)));
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
                branch_lineage: Vec::new(),
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
                branch_lineage: Vec::new(),
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
        assert_eq!(
            event,
            RoutingEvent::ReleaseRemoved {
                namespace: removed.namespace,
                service: removed.service,
            }
        );
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
                branch_lineage: Vec::new(),
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
                branch_lineage: Vec::new(),
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
                branch_lineage: Vec::new(),
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
    async fn list_volumes_returns_contract_identity_order() {
        let store = MemoryStore::new();
        let namespace = Namespace("prod".into());
        let deploy_id = DeployId("deploy-1".into());

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: Vec::new(),
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: Vec::new(),
                releases: Vec::new(),
                volumes: vec![
                    test_volume(&namespace, "z-data", &deploy_id),
                    test_volume(&namespace, "a-data", &deploy_id),
                ],
                deploy: test_deploy(&namespace, "deploy-prod"),
            })
            .await
            .expect("seed volumes");

        let volumes = store.list_volumes(&namespace).await.expect("list volumes");

        assert_eq!(
            volumes
                .iter()
                .map(|volume| volume.volume_name.as_str())
                .collect::<Vec<_>>(),
            ["a-data", "z-data"]
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
            Some(RoutingEvent::InstanceUpsert(InstanceStatusRecord { .. }))
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
            Some(RoutingEvent::InstanceRemoved { instance_id }) if instance_id == status.instance_id
        ));

        let snapshot = store
            .load_routing_state()
            .await
            .expect("load routing state");
        assert!(snapshot.instances.is_empty());
    }

    #[tokio::test]
    async fn instance_status_list_returns_contract_identity_order() {
        let store = MemoryStore::new();
        let namespace = Namespace("prod".into());
        store
            .record_instance_status(&test_instance_status(&namespace, "instance-b"))
            .await
            .expect("record instance b");
        store
            .record_instance_status(&test_instance_status(&namespace, "instance-a"))
            .await
            .expect("record instance a");

        let records = store
            .list_instance_status(&namespace)
            .await
            .expect("list instance status");

        assert_eq!(
            records
                .iter()
                .map(|record| record.instance_id.0.as_str())
                .collect::<Vec<_>>(),
            ["instance-a", "instance-b"]
        );
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

    #[tokio::test]
    async fn certificate_lists_and_snapshots_return_contract_identity_order() {
        let store = MemoryStore::new();
        store
            .upsert_certificate(&test_certificate("z.example.com"))
            .await
            .expect("seed z cert");
        store
            .upsert_certificate(&test_certificate("a.example.com"))
            .await
            .expect("seed a cert");
        store
            .upsert_acme_challenge(&test_acme_challenge("z.example.com", "token-b"))
            .await
            .expect("seed z challenge");
        store
            .upsert_acme_challenge(&test_acme_challenge("a.example.com", "token-a"))
            .await
            .expect("seed a challenge");
        for machine_id in ["machine-b", "machine-a"] {
            store
                .upsert_acme_challenge_readiness(&AcmeChallengeReadinessRecord {
                    hostname: "example.com".into(),
                    token: "token".into(),
                    machine_id: MachineId(machine_id.into()),
                    observed_at: 1,
                })
                .await
                .expect("seed readiness");
        }

        let certificates = store.list_certificates().await.expect("list certificates");
        assert_eq!(
            certificates
                .iter()
                .map(|record| record.hostname.as_str())
                .collect::<Vec<_>>(),
            ["a.example.com", "z.example.com"]
        );
        let (certificate_snapshot, _events) = store
            .subscribe_certificates()
            .await
            .expect("subscribe certificates");
        assert_eq!(
            certificate_snapshot
                .iter()
                .map(|record| record.hostname.as_str())
                .collect::<Vec<_>>(),
            ["a.example.com", "z.example.com"]
        );

        let challenges = store.list_acme_challenges().await.expect("list challenges");
        assert_eq!(
            challenges
                .iter()
                .map(|record| (record.hostname.as_str(), record.token.as_str()))
                .collect::<Vec<_>>(),
            [("a.example.com", "token-a"), ("z.example.com", "token-b")]
        );
        let (challenge_snapshot, _events) = store
            .subscribe_acme_challenges()
            .await
            .expect("subscribe challenges");
        assert_eq!(
            challenge_snapshot
                .iter()
                .map(|record| (record.hostname.as_str(), record.token.as_str()))
                .collect::<Vec<_>>(),
            [("a.example.com", "token-a"), ("z.example.com", "token-b")]
        );

        let readiness = store
            .list_acme_challenge_readiness("example.com", "token")
            .await
            .expect("list readiness");
        assert_eq!(
            readiness
                .iter()
                .map(|record| record.machine_id.0.as_str())
                .collect::<Vec<_>>(),
            ["machine-a", "machine-b"]
        );
    }

    #[tokio::test]
    async fn certificate_subscribers_receive_snapshot_and_updates() {
        let store = MemoryStore::new();
        let mut certificate = test_certificate("example.com");
        store
            .upsert_certificate(&certificate)
            .await
            .expect("seed certificate");

        let (snapshot, mut events) = store
            .subscribe_certificates()
            .await
            .expect("subscribe certificates");
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].hostname, "example.com");

        certificate.last_error = Some("renewal failed".into());
        store
            .upsert_certificate(&certificate)
            .await
            .expect("update certificate");

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
            .await
            .expect("certificate update event deadline")
            .expect("certificate update event")
            .expect("certificate update should be successful");
        let CertificateEvent::Upsert(updated) = event else {
            panic!("expected certificate update event, got {event:?}");
        };
        assert_eq!(updated.hostname, "example.com");
        assert_eq!(updated.last_error.as_deref(), Some("renewal failed"));
    }

    #[tokio::test]
    async fn acme_challenge_subscribers_receive_snapshot_updates_and_removals() {
        let store = MemoryStore::new();
        let mut challenge = test_acme_challenge("example.com", "token-1");
        store
            .upsert_acme_challenge(&challenge)
            .await
            .expect("seed challenge");

        let (snapshot, mut events) = store
            .subscribe_acme_challenges()
            .await
            .expect("subscribe challenges");
        assert_eq!(snapshot, vec![challenge.clone()]);

        challenge.key_authorization = "updated-key-auth".into();
        store
            .upsert_acme_challenge(&challenge)
            .await
            .expect("update challenge");
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
            .await
            .expect("challenge update event deadline")
            .expect("challenge update event")
            .expect("challenge update should be successful");
        let AcmeChallengeEvent::Upsert(updated) = event else {
            panic!("expected challenge update event, got {event:?}");
        };
        assert_eq!(updated.key_authorization, "updated-key-auth");

        store
            .delete_acme_challenge("example.com", "token-1")
            .await
            .expect("delete challenge");
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
            .await
            .expect("challenge removal event deadline")
            .expect("challenge removal event")
            .expect("challenge removal should be successful");
        let AcmeChallengeEvent::Removed { hostname, token } = event else {
            panic!("expected challenge removal event, got {event:?}");
        };
        assert_eq!(hostname, "example.com");
        assert_eq!(token, "token-1");

        assert!(
            store
                .list_acme_challenges()
                .await
                .expect("list challenges")
                .is_empty()
        );
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

    fn test_deploy_phase(
        namespace: &Namespace,
        deploy_id: &DeployId,
        phase_id: &str,
        order: u32,
    ) -> DeployPhaseRecord {
        DeployPhaseRecord {
            namespace: namespace.clone(),
            deploy_id: deploy_id.clone(),
            phase_id: ployz_types::model::DeployPhaseId(phase_id.into()),
            name: phase_id.into(),
            order,
            state: DeployPhaseState::Running,
            commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
            rollback_policy: DeployPhaseRollbackPolicy::Reversible,
            started_at: 10,
        }
    }

    fn test_revision(
        namespace: &Namespace,
        service: &str,
        revision_hash: &str,
    ) -> ServiceRevisionRecord {
        ServiceRevisionRecord {
            namespace: namespace.clone(),
            service: service.into(),
            revision_hash: revision_hash.into(),
            spec_json: "{}".into(),
            created_by: MachineId("machine-1".into()),
            created_at: 1,
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

    fn test_certificate(hostname: &str) -> CertificateRecord {
        CertificateRecord {
            hostname: hostname.into(),
            issuer_url: "https://acme.example/directory".into(),
            account_id: "account-1".into(),
            state: ployz_types::model::CertificateState::Pending,
            active_version_id: None,
            versions: Vec::new(),
            order_url: None,
            last_error: None,
            requested_at: 1,
            updated_at: 1,
            next_renewal_at: None,
        }
    }

    fn test_acme_challenge(hostname: &str, token: &str) -> AcmeChallengeRecord {
        AcmeChallengeRecord {
            hostname: hostname.into(),
            token: token.into(),
            key_authorization: "key-auth".into(),
            expires_at: 100,
            created_at: 1,
        }
    }

    #[tokio::test]
    async fn load_routing_state_includes_machines() {
        let store = MemoryStore::new();
        let machine = MachineMembership {
            id: MachineId("machine-1".into()),
            public_key: ployz_types::model::PublicKey([0; 32]),
            overlay_ip: ployz_types::model::OverlayIp("fd00::1".parse().expect("valid overlay")),
            region_role: ployz_types::model::RegionRole::HomeData,
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
    async fn machine_lists_and_routing_snapshots_return_machine_id_order() {
        let store = MemoryStore::new();
        store
            .upsert_self_machine(&test_machine("machine-b"))
            .await
            .expect("insert machine b");
        store
            .upsert_self_machine(&test_machine("machine-a"))
            .await
            .expect("insert machine a");

        let listed = store.list_machines().await.expect("list machines");
        assert_eq!(
            listed
                .iter()
                .map(|machine| machine.id.0.as_str())
                .collect::<Vec<_>>(),
            ["machine-a", "machine-b"]
        );

        let routing = store.load_routing_state().await.expect("load routing");
        assert_eq!(
            routing
                .machines
                .iter()
                .map(|machine| machine.id.0.as_str())
                .collect::<Vec<_>>(),
            ["machine-a", "machine-b"]
        );

        let (snapshot, _events) = store
            .subscribe_machines()
            .await
            .expect("subscribe machines");
        assert_eq!(
            snapshot
                .iter()
                .map(|machine| machine.id.0.as_str())
                .collect::<Vec<_>>(),
            ["machine-a", "machine-b"]
        );
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
            region_role: ployz_types::model::RegionRole::HomeData,
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
            Some(RoutingEvent::MachineUpsert(MachineMembership { .. }))
        ));
    }

    #[tokio::test]
    async fn delete_machine_updates_routing_snapshot_and_emits_removal_events() {
        let store = MemoryStore::new();
        let machine = test_machine("machine-1");
        store
            .upsert_self_machine(&machine)
            .await
            .expect("seed machine");
        let (_machine_snapshot, mut machine_rx) = store
            .subscribe_machines()
            .await
            .expect("subscribe machines");
        let (_routing_state, mut routing_rx) = store
            .subscribe_routing_events()
            .await
            .expect("subscribe routing");

        store
            .delete_machine(&machine.id)
            .await
            .expect("delete machine");

        let machine_event =
            tokio::time::timeout(std::time::Duration::from_secs(1), machine_rx.recv())
                .await
                .expect("machine removal event deadline")
                .expect("machine removal event")
                .expect("machine removal event should be successful");
        let MachineEvent::Removed { id } = machine_event else {
            panic!("expected machine removal event, got {machine_event:?}");
        };
        assert_eq!(id, machine.id);

        let routing_event =
            tokio::time::timeout(std::time::Duration::from_secs(1), routing_rx.recv())
                .await
                .expect("routing removal event deadline")
                .expect("routing removal event");
        assert_eq!(
            routing_event,
            RoutingEvent::MachineRemoved { id: machine.id }
        );

        let state = store.load_routing_state().await.expect("routing state");
        assert!(state.machines.is_empty());
    }

    #[tokio::test]
    async fn routing_event_subscription_returns_snapshot_then_metadata_rich_events() {
        let store = MemoryStore::new();
        let machine = test_machine("machine-1");
        store
            .upsert_self_machine(&machine)
            .await
            .expect("seed machine");

        let (state, mut envelopes) = store
            .subscribe_routing_events_internal()
            .await
            .expect("subscribe routing events");

        assert_eq!(state.machines, vec![machine.clone()]);

        let mut updated = machine.clone();
        updated.labels.insert("role".into(), "gateway".into());
        store
            .upsert_self_machine(&updated)
            .await
            .expect("update machine");

        let envelope = tokio::time::timeout(std::time::Duration::from_secs(1), envelopes.recv())
            .await
            .expect("routing event deadline")
            .expect("routing event")
            .expect("routing event should be successful");

        assert_eq!(envelope.event_id, "memory:machine:machine-1:1");
        assert_eq!(envelope.cause.as_deref(), Some("machine.upsert"));
        let RoutingEvent::MachineUpsert(new) = &envelope.event else {
            panic!("expected machine upsert event, got {:?}", envelope.event);
        };
        assert_eq!(new.labels.get("role").map(String::as_str), Some("gateway"));

        envelope
            .ack()
            .await
            .expect("memory routing event ack is a no-op");
    }

    #[tokio::test]
    async fn lagged_routing_broadcast_reports_error_then_closes() {
        let (sender, receiver) = broadcast::channel(2);
        let mut event_rx = subscribe_routing_broadcast(receiver);
        let event = RoutingEvent::ReleaseRemoved {
            namespace: Namespace("prod".into()),
            service: "api".into(),
        };

        for index in 0..3 {
            sender
                .send(MemoryRoutingEvent {
                    event_id: format!("event-{index}"),
                    cause: Some("test".into()),
                    event: event.clone(),
                })
                .expect("subscriber should be present");
        }

        let error = event_rx
            .recv()
            .await
            .expect("lag error should be delivered")
            .expect_err("lagged routing stream should report an error");
        assert_eq!(
            error,
            Error::SubscriptionLagged {
                stream: SubscriptionStream::Routing,
                skipped: 1
            }
        );
        assert!(event_rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn lagged_event_broadcast_reports_error_then_closes() {
        let (sender, receiver) = broadcast::channel(2);
        let mut event_rx = subscribe_broadcast(SubscriptionStream::Machine, receiver);

        for index in 0..3 {
            sender
                .send(MachineEvent::Upsert(test_machine(format!(
                    "machine-{index}"
                ))))
                .expect("subscriber should be present");
        }

        let error = event_rx
            .recv()
            .await
            .expect("lag error should be delivered")
            .expect_err("lagged stream should report an error");
        assert_eq!(
            error,
            Error::SubscriptionLagged {
                stream: SubscriptionStream::Machine,
                skipped: 1
            }
        );
        assert!(event_rx.recv().await.is_none());
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
                branch_lineage: Vec::new(),
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
        store
            .upsert_deploy_phase(&test_deploy_phase(
                &namespace,
                &DeployId("dep-1".into()),
                "deploy",
                0,
            ))
            .await
            .expect("write phase before wipe");

        store.wipe_data().await.expect("wipe data");

        assert!(
            store
                .list_volumes(&namespace)
                .await
                .expect("list volumes after wipe")
                .is_empty()
        );
        assert!(
            store
                .list_deploy_phases(&namespace, &DeployId("dep-1".into()))
                .await
                .expect("list phases after wipe")
                .is_empty()
        );
    }
}
