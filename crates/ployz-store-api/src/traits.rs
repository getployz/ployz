use async_trait::async_trait;
use ployz_types::Result;
use ployz_types::error::Error;
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeEvent, AcmeChallengeReadinessRecord, AcmeChallengeRecord,
    CertificateEvent, CertificateRecord, DeployId, DeployRecord, InstanceId, InstanceStatusRecord,
    InviteRecord, MachineEvent, MachineId, MachineMembership, RoutingEvent, RoutingState,
    ServiceReleaseRecord, ServiceRevisionRecord, VolumeRecord,
};
use ployz_types::spec::Namespace;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::net::SocketAddr;
use tokio::sync::{mpsc, oneshot};

pub type MachineSubscription = (Vec<MachineMembership>, mpsc::Receiver<MachineEvent>);
pub type CertificateSubscription = (Vec<CertificateRecord>, mpsc::Receiver<CertificateEvent>);
pub type AcmeChallengeSubscription = (Vec<AcmeChallengeRecord>, mpsc::Receiver<AcmeChallengeEvent>);
pub type RoutingBatchSubscription = (RoutingState, mpsc::Receiver<RoutingEventBatch>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingSubscription {
    Durable { consumer_id: String },
    Temporary { consumer_id: String },
}

impl RoutingSubscription {
    #[must_use]
    pub fn durable(consumer_id: impl Into<String>) -> Self {
        Self::Durable {
            consumer_id: consumer_id.into(),
        }
    }

    #[must_use]
    pub fn temporary(consumer_id: impl Into<String>) -> Self {
        Self::Temporary {
            consumer_id: consumer_id.into(),
        }
    }

    #[must_use]
    pub fn consumer_id(&self) -> &str {
        match self {
            Self::Durable { consumer_id } | Self::Temporary { consumer_id } => consumer_id,
        }
    }

    #[must_use]
    pub fn delete_on_close(&self) -> bool {
        matches!(self, Self::Temporary { .. })
    }
}

#[derive(Debug)]
pub struct RoutingEventBatch {
    pub batch_id: String,
    pub cause: Option<String>,
    pub events: Vec<RoutingEvent>,
    ack: Option<oneshot::Sender<()>>,
}

impl RoutingEventBatch {
    #[must_use]
    pub fn unacked(
        batch_id: impl Into<String>,
        cause: Option<String>,
        events: Vec<RoutingEvent>,
    ) -> Self {
        Self {
            batch_id: batch_id.into(),
            cause,
            events,
            ack: None,
        }
    }

    #[must_use]
    pub fn with_ack(
        batch_id: impl Into<String>,
        cause: Option<String>,
        events: Vec<RoutingEvent>,
        ack: oneshot::Sender<()>,
    ) -> Self {
        Self {
            batch_id: batch_id.into(),
            cause,
            events,
            ack: Some(ack),
        }
    }

    pub async fn ack(mut self) -> Result<()> {
        let Some(ack) = self.ack.take() else {
            return Ok(());
        };
        ack.send(()).map_err(|()| {
            Error::operation(
                "routing_batch_ack_failed",
                format!("routing batch '{}' ack receiver closed", self.batch_id),
            )
        })
    }
}

pub fn apply_routing_event(state: &mut RoutingState, event: RoutingEvent) {
    match event {
        RoutingEvent::MachineAdded(record) | RoutingEvent::MachineUpdated { new: record, .. } => {
            upsert_by(&mut state.machines, record, |record| record.id.clone());
        }
        RoutingEvent::MachineRemoved(record) => {
            state.machines.retain(|candidate| candidate.id != record.id);
        }
        RoutingEvent::RevisionAdded(record) | RoutingEvent::RevisionUpdated { new: record, .. } => {
            upsert_by(&mut state.revisions, record, |record| {
                (
                    record.namespace.clone(),
                    record.service.clone(),
                    record.revision_hash.clone(),
                )
            });
        }
        RoutingEvent::RevisionRemoved(record) => {
            state.revisions.retain(|candidate| {
                candidate.namespace != record.namespace
                    || candidate.service != record.service
                    || candidate.revision_hash != record.revision_hash
            });
        }
        RoutingEvent::ReleaseAdded(record) | RoutingEvent::ReleaseUpdated { new: record, .. } => {
            upsert_by(&mut state.releases, record, |record| {
                (record.namespace.clone(), record.service.clone())
            });
        }
        RoutingEvent::ReleaseRemoved(record) => {
            state.releases.retain(|candidate| {
                candidate.namespace != record.namespace || candidate.service != record.service
            });
        }
        RoutingEvent::InstanceAdded(record) | RoutingEvent::InstanceUpdated { new: record, .. } => {
            upsert_by(&mut state.instances, record, |record| {
                record.instance_id.clone()
            });
        }
        RoutingEvent::InstanceRemoved(record) => {
            state
                .instances
                .retain(|candidate| candidate.instance_id != record.instance_id);
        }
    }
}

pub fn apply_routing_events(
    state: &mut RoutingState,
    events: impl IntoIterator<Item = RoutingEvent>,
) {
    for event in events {
        apply_routing_event(state, event);
    }
}

fn upsert_by<T, K>(items: &mut Vec<T>, value: T, key: impl Fn(&T) -> K)
where
    K: PartialEq,
{
    let value_key = key(&value);
    if let Some(existing) = items
        .iter_mut()
        .find(|candidate| key(candidate) == value_key)
    {
        *existing = value;
    } else {
        items.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::RoutingSubscription;

    #[test]
    fn routing_subscription_kind_controls_consumer_cleanup() {
        let durable = RoutingSubscription::durable("gateway.founder");
        let temporary = RoutingSubscription::temporary("ployzd.runtime.founder.1");

        assert_eq!(durable.consumer_id(), "gateway.founder");
        assert!(!durable.delete_on_close());
        assert_eq!(temporary.consumer_id(), "ployzd.runtime.founder.1");
        assert!(temporary.delete_on_close());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployCommit {
    pub namespace: Namespace,
    pub revisions: Vec<ServiceRevisionRecord>,
    pub removed_services: Vec<String>,
    pub removed_volumes: Vec<String>,
    pub releases: Vec<ServiceReleaseRecord>,
    pub volumes: Vec<VolumeRecord>,
    pub deploy: DeployRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployRevisionUpsert {
    pub revision: ServiceRevisionRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployRecordUpdate {
    pub deploy: DeployRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploySnapshot {
    pub revisions: Vec<ServiceRevisionRecord>,
    pub releases: Vec<ServiceReleaseRecord>,
    pub instances: Vec<InstanceStatusRecord>,
}

pub trait MachineRegistry: Send + Sync {
    fn init(&self) -> impl Future<Output = Result<()>> + Send + '_ {
        async { Ok(()) }
    }

    fn list_machines(&self) -> impl Future<Output = Result<Vec<MachineMembership>>> + Send + '_;

    fn upsert_self_machine<'a>(
        &'a self,
        record: &'a MachineMembership,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn delete_machine<'a>(
        &'a self,
        id: &'a MachineId,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn subscribe_machines(&self) -> impl Future<Output = Result<MachineSubscription>> + Send + '_;
}

pub trait InviteRepository: Send + Sync {
    fn create_invite<'a>(
        &'a self,
        invite: &'a InviteRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn get_invite<'a>(
        &'a self,
        invite_id: &'a str,
    ) -> impl Future<Output = Result<Option<InviteRecord>>> + Send + 'a;

    fn list_invites(&self) -> impl Future<Output = Result<Vec<InviteRecord>>> + Send + '_;

    fn redeem_invite<'a>(
        &'a self,
        invite_id: &'a str,
        machine_id: &'a MachineId,
        now_unix_secs: u64,
    ) -> impl Future<Output = Result<InviteRecord>> + Send + 'a;

    fn revoke_invite<'a>(
        &'a self,
        invite_id: &'a str,
        now_unix_secs: u64,
    ) -> impl Future<Output = Result<InviteRecord>> + Send + 'a;
}

pub trait RoutingSnapshotReader: Send + Sync {
    fn load_routing_state(&self) -> impl Future<Output = Result<RoutingState>> + Send + '_;

    fn subscribe_routing_batches<'a>(
        &'a self,
        subscription: RoutingSubscription,
    ) -> impl Future<Output = Result<RoutingBatchSubscription>> + Send + 'a;
}

pub trait DeployRepository: Send + Sync {
    fn list_deploy_releases<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<Vec<ServiceReleaseRecord>>> + Send + 'a;

    fn load_deploy_snapshot<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<DeploySnapshot>> + Send + 'a;

    fn list_volumes<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<Vec<VolumeRecord>>> + Send + 'a;

    fn get_volume<'a>(
        &'a self,
        namespace: &'a Namespace,
        volume_name: &'a str,
    ) -> impl Future<Output = Result<Option<VolumeRecord>>> + Send + 'a;

    fn record_service_revision<'a>(
        &'a self,
        command: &'a DeployRevisionUpsert,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn commit_deploy<'a>(
        &'a self,
        command: &'a DeployCommit,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn update_deploy_record<'a>(
        &'a self,
        command: &'a DeployRecordUpdate,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn get_deploy<'a>(
        &'a self,
        deploy_id: &'a DeployId,
    ) -> impl Future<Output = Result<Option<DeployRecord>>> + Send + 'a;
}

pub trait InstanceStatusRepository: Send + Sync {
    fn list_instance_status<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<Vec<InstanceStatusRecord>>> + Send + 'a;

    fn record_instance_status<'a>(
        &'a self,
        record: &'a InstanceStatusRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn remove_instance_status<'a>(
        &'a self,
        instance_id: &'a InstanceId,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
}

pub trait CertificateStore: Send + Sync {
    fn get_acme_account<'a>(
        &'a self,
        issuer_url: &'a str,
    ) -> impl Future<Output = Result<Option<AcmeAccountRecord>>> + Send + 'a;

    fn upsert_acme_account<'a>(
        &'a self,
        record: &'a AcmeAccountRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn list_certificates(&self)
    -> impl Future<Output = Result<Vec<CertificateRecord>>> + Send + '_;

    fn get_certificate<'a>(
        &'a self,
        hostname: &'a str,
    ) -> impl Future<Output = Result<Option<CertificateRecord>>> + Send + 'a;

    fn upsert_certificate<'a>(
        &'a self,
        record: &'a CertificateRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn list_acme_challenges(
        &self,
    ) -> impl Future<Output = Result<Vec<AcmeChallengeRecord>>> + Send + '_;

    fn upsert_acme_challenge<'a>(
        &'a self,
        record: &'a AcmeChallengeRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn delete_acme_challenge<'a>(
        &'a self,
        hostname: &'a str,
        token: &'a str,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn subscribe_certificates(
        &self,
    ) -> impl Future<Output = Result<CertificateSubscription>> + Send + '_;

    fn subscribe_acme_challenges(
        &self,
    ) -> impl Future<Output = Result<AcmeChallengeSubscription>> + Send + '_;

    fn upsert_acme_challenge_readiness<'a>(
        &'a self,
        record: &'a AcmeChallengeReadinessRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn list_acme_challenge_readiness<'a>(
        &'a self,
        hostname: &'a str,
        token: &'a str,
    ) -> impl Future<Output = Result<Vec<AcmeChallengeReadinessRecord>>> + Send + 'a;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    Disconnected,
    Syncing { gaps: u64 },
    Synced,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRttObservation {
    pub addr: SocketAddr,
    pub rtts_ms: Vec<u64>,
}

pub trait PeerRttStore: Send + Sync {
    fn peer_rtt_observations(
        &self,
    ) -> impl Future<Output = Result<Vec<PeerRttObservation>>> + Send + '_ {
        async { Ok(Vec::new()) }
    }
}

pub trait SyncProbe: Send + Sync {
    fn sync_status(&self) -> impl Future<Output = Result<SyncStatus>> + Send + '_ {
        async { Ok(SyncStatus::Synced) }
    }
}

#[async_trait]
pub trait StoreRuntimeControl: Send + Sync {
    async fn start(&self) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    async fn wipe_data(&self) -> Result<()> {
        Ok(())
    }
    async fn healthy(&self) -> bool;
}
