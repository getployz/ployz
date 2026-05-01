use async_nats::jetstream::consumer::push;
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy};
use async_nats::jetstream::kv;
use async_trait::async_trait;
use futures_util::StreamExt;
use ployz_store_api::{
    AcmeChallengeSubscription, CertificateStore, CertificateSubscription, DeployCommit,
    DeployRecordUpdate, DeployRepository, DeployRevisionUpsert, DeploySnapshot,
    InstanceStatusRepository, InviteRepository, MachineRegistry, MachineSubscription,
    PeerMembershipObservation, PeerMembershipStore, PeerRttObservation, PeerRttStore,
    RoutingSnapshotReader, RoutingSubscription, StoreBackend, StoreRuntimeControl, SyncProbe,
    SyncStatus,
};
use ployz_types::Result;
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeRecord, CertificateRecord, DeployId, DeployRecord, InstanceId,
    InstanceStatusRecord, InviteRecord, MachineId, MachineMembership, RoutingEvent, RoutingState,
    ServiceReleaseRecord, VolumeRecord,
};
use ployz_types::spec::Namespace;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::warn;

use crate::NatsStore;
use crate::buckets::{INSTANCES_BUCKET, MACHINES_BUCKET, ensure_assets};
use crate::store::instances::list_all_instance_status;
use crate::store::kv_json;
use crate::subjects::DEPLOY_COMMITS_STREAM;

#[async_trait]
impl StoreBackend for NatsStore {
    async fn init(&self) -> Result<()> {
        ensure_assets(self.jetstream(), self.asset_policy()).await
    }

    async fn list_machines(&self) -> Result<Vec<MachineMembership>> {
        MachineRegistry::list_machines(self).await
    }

    async fn upsert_self_machine(&self, record: &MachineMembership) -> Result<()> {
        MachineRegistry::upsert_self_machine(self, record).await
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        MachineRegistry::delete_machine(self, id).await
    }

    async fn subscribe_machines(&self) -> Result<MachineSubscription> {
        MachineRegistry::subscribe_machines(self).await
    }

    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        InviteRepository::create_invite(self, invite).await
    }

    async fn get_invite(&self, invite_id: &str) -> Result<Option<InviteRecord>> {
        InviteRepository::get_invite(self, invite_id).await
    }

    async fn list_invites(&self) -> Result<Vec<InviteRecord>> {
        InviteRepository::list_invites(self).await
    }

    async fn redeem_invite(
        &self,
        invite_id: &str,
        machine_id: &MachineId,
        now_unix_secs: u64,
    ) -> Result<InviteRecord> {
        InviteRepository::redeem_invite(self, invite_id, machine_id, now_unix_secs).await
    }

    async fn revoke_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<InviteRecord> {
        InviteRepository::revoke_invite(self, invite_id, now_unix_secs).await
    }

    async fn load_routing_state(&self) -> Result<RoutingState> {
        RoutingSnapshotReader::load_routing_state(self).await
    }

    async fn subscribe_routing_events(&self) -> Result<RoutingSubscription> {
        RoutingSnapshotReader::subscribe_routing_events(self).await
    }

    async fn list_deploy_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        DeployRepository::list_deploy_releases(self, namespace).await
    }

    async fn load_deploy_snapshot(&self, namespace: &Namespace) -> Result<DeploySnapshot> {
        DeployRepository::load_deploy_snapshot(self, namespace).await
    }

    async fn list_volumes(&self, namespace: &Namespace) -> Result<Vec<VolumeRecord>> {
        DeployRepository::list_volumes(self, namespace).await
    }

    async fn get_volume(
        &self,
        namespace: &Namespace,
        volume_name: &str,
    ) -> Result<Option<VolumeRecord>> {
        DeployRepository::get_volume(self, namespace, volume_name).await
    }

    async fn record_service_revision(&self, command: &DeployRevisionUpsert) -> Result<()> {
        DeployRepository::record_service_revision(self, command).await
    }

    async fn commit_deploy(&self, command: &DeployCommit) -> Result<()> {
        DeployRepository::commit_deploy(self, command).await
    }

    async fn update_deploy_record(&self, command: &DeployRecordUpdate) -> Result<()> {
        DeployRepository::update_deploy_record(self, command).await
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        DeployRepository::get_deploy(self, deploy_id).await
    }

    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        InstanceStatusRepository::list_instance_status(self, namespace).await
    }

    async fn record_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        InstanceStatusRepository::record_instance_status(self, record).await
    }

    async fn remove_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        InstanceStatusRepository::remove_instance_status(self, instance_id).await
    }

    async fn get_acme_account(&self, issuer_url: &str) -> Result<Option<AcmeAccountRecord>> {
        CertificateStore::get_acme_account(self, issuer_url).await
    }

    async fn upsert_acme_account(&self, record: &AcmeAccountRecord) -> Result<()> {
        CertificateStore::upsert_acme_account(self, record).await
    }

    async fn list_certificates(&self) -> Result<Vec<CertificateRecord>> {
        CertificateStore::list_certificates(self).await
    }

    async fn get_certificate(&self, hostname: &str) -> Result<Option<CertificateRecord>> {
        CertificateStore::get_certificate(self, hostname).await
    }

    async fn upsert_certificate(&self, record: &CertificateRecord) -> Result<()> {
        CertificateStore::upsert_certificate(self, record).await
    }

    async fn list_acme_challenges(&self) -> Result<Vec<AcmeChallengeRecord>> {
        CertificateStore::list_acme_challenges(self).await
    }

    async fn upsert_acme_challenge(&self, record: &AcmeChallengeRecord) -> Result<()> {
        CertificateStore::upsert_acme_challenge(self, record).await
    }

    async fn delete_acme_challenge(&self, hostname: &str, token: &str) -> Result<()> {
        CertificateStore::delete_acme_challenge(self, hostname, token).await
    }

    async fn subscribe_certificates(&self) -> Result<CertificateSubscription> {
        CertificateStore::subscribe_certificates(self).await
    }

    async fn subscribe_acme_challenges(&self) -> Result<AcmeChallengeSubscription> {
        CertificateStore::subscribe_acme_challenges(self).await
    }

    async fn sync_status(&self) -> Result<SyncStatus> {
        SyncProbe::sync_status(self).await
    }

    async fn peer_rtt_observations(&self) -> Result<Vec<PeerRttObservation>> {
        PeerRttStore::peer_rtt_observations(self).await
    }

    async fn peer_membership_observations(&self) -> Result<Vec<PeerMembershipObservation>> {
        PeerMembershipStore::peer_membership_observations(self).await
    }
}

impl RoutingSnapshotReader for NatsStore {
    async fn load_routing_state(&self) -> Result<RoutingState> {
        let projection = self.deploy_projection_snapshot().await?;
        Ok(RoutingState {
            machines: MachineRegistry::list_machines(self).await?,
            revisions: projection.all_revisions(),
            releases: projection.all_releases(),
            instances: list_all_instance_status(self).await?,
        })
    }

    async fn subscribe_routing_events(&self) -> Result<RoutingSubscription> {
        let machines_bucket =
            kv_json::get_bucket(self.jetstream(), MACHINES_BUCKET, "nats_machines_bucket").await?;
        let instances_bucket =
            kv_json::get_bucket(self.jetstream(), INSTANCES_BUCKET, "nats_instances_bucket")
                .await?;
        let mut machine_watch = machines_bucket.watch_all().await.map_err(|error| {
            ployz_types::error::Error::operation("nats_machines_watch", format!("{error:?}"))
        })?;
        let mut instance_watch = instances_bucket.watch_all().await.map_err(|error| {
            ployz_types::error::Error::operation("nats_instances_watch", format!("{error:?}"))
        })?;
        let deploy_stream = self
            .jetstream()
            .get_stream(DEPLOY_COMMITS_STREAM)
            .await
            .map_err(|error| {
                ployz_types::error::Error::operation("nats_deploy_stream", format!("{error:?}"))
            })?;
        let consumer: async_nats::jetstream::consumer::PushConsumer = deploy_stream
            .create_consumer(push::Config {
                deliver_subject: self.client().new_inbox(),
                deliver_policy: DeliverPolicy::New,
                ack_policy: AckPolicy::Explicit,
                inactive_threshold: Duration::from_secs(60),
                ..Default::default()
            })
            .await
            .map_err(|error| {
                ployz_types::error::Error::operation("nats_deploy_consumer", format!("{error:?}"))
            })?;
        let mut deploy_messages = consumer.messages().await.map_err(|error| {
            ployz_types::error::Error::operation("nats_deploy_messages", format!("{error:?}"))
        })?;
        let state = RoutingSnapshotReader::load_routing_state(self).await?;
        let mut machines = state
            .machines
            .iter()
            .map(|record| (record.id.0.clone(), record.clone()))
            .collect::<HashMap<_, _>>();
        let mut instances = state
            .instances
            .iter()
            .map(|record| (record.instance_id.0.clone(), record.clone()))
            .collect::<HashMap<_, _>>();
        let deploy_projection = self.deploy_projection.clone();
        let (tx, rx) = mpsc::channel(128);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    next = machine_watch.next() => {
                        let Some(next) = next else { break };
                        match next {
                            Ok(entry) => {
                                if let Some(event) = machine_routing_event(entry, &mut machines) {
                                    if tx.send(event).await.is_err() {
                                        break;
                                    }
                                }
                            }
                            Err(error) => {
                                warn!(?error, "NATS routing machine watcher failed");
                                break;
                            }
                        }
                    }
                    next = instance_watch.next() => {
                        let Some(next) = next else { break };
                        match next {
                            Ok(entry) => {
                                if let Some(event) = instance_routing_event(entry, &mut instances) {
                                    if tx.send(event).await.is_err() {
                                        break;
                                    }
                                }
                            }
                            Err(error) => {
                                warn!(?error, "NATS routing instance watcher failed");
                                break;
                            }
                        }
                    }
                    next = deploy_messages.next() => {
                        let Some(next) = next else { break };
                        match next {
                            Ok(message) => {
                                let commit = match serde_json::from_slice::<DeployCommit>(message.payload.as_ref()) {
                                    Ok(commit) => commit,
                                    Err(error) => {
                                        warn!(?error, "NATS deploy commit routing event decode failed");
                                        let _ = message.ack().await;
                                        continue;
                                    }
                                };
                                let events = {
                                    let mut guard = deploy_projection.write().await;
                                    let projection = guard.get_or_insert_with(Default::default);
                                    projection.apply_commit_events(&commit)
                                };
                                let _ = message.ack().await;
                                for event in events {
                                    if tx.send(event).await.is_err() {
                                        return;
                                    }
                                }
                            }
                            Err(error) => {
                                warn!(?error, "NATS deploy commit routing consumer failed");
                                break;
                            }
                        }
                    }
                }
            }
        });
        Ok((state, rx))
    }
}

fn machine_routing_event(
    entry: kv::Entry,
    machines: &mut HashMap<String, MachineMembership>,
) -> Option<RoutingEvent> {
    match entry.operation {
        kv::Operation::Put => {
            let record = match kv_json::decode_json::<MachineMembership>(
                "nats_machine_decode",
                entry.value.as_ref(),
            ) {
                Ok(record) => record,
                Err(error) => {
                    warn!(?error, key = %entry.key, "NATS machine routing event decode failed");
                    return None;
                }
            };
            match machines.insert(entry.key, record.clone()) {
                Some(old) if old != record => {
                    Some(RoutingEvent::MachineUpdated { old, new: record })
                }
                Some(_) => None,
                None => Some(RoutingEvent::MachineAdded(record)),
            }
        }
        kv::Operation::Delete | kv::Operation::Purge => machines
            .remove(&entry.key)
            .map(RoutingEvent::MachineRemoved),
    }
}

fn instance_routing_event(
    entry: kv::Entry,
    instances: &mut HashMap<String, InstanceStatusRecord>,
) -> Option<RoutingEvent> {
    match entry.operation {
        kv::Operation::Put => {
            let record = match kv_json::decode_json::<InstanceStatusRecord>(
                "nats_instance_decode",
                entry.value.as_ref(),
            ) {
                Ok(record) => record,
                Err(error) => {
                    warn!(?error, key = %entry.key, "NATS instance routing event decode failed");
                    return None;
                }
            };
            match instances.insert(entry.key, record.clone()) {
                Some(old) if old != record => {
                    Some(RoutingEvent::InstanceUpdated { old, new: record })
                }
                Some(_) => None,
                None => Some(RoutingEvent::InstanceAdded(record)),
            }
        }
        kv::Operation::Delete | kv::Operation::Purge => instances
            .remove(&entry.key)
            .map(RoutingEvent::InstanceRemoved),
    }
}

impl SyncProbe for NatsStore {
    async fn sync_status(&self) -> Result<SyncStatus> {
        Ok(SyncStatus::Synced)
    }
}

impl PeerRttStore for NatsStore {}

impl PeerMembershipStore for NatsStore {}

#[async_trait]
impl StoreRuntimeControl for NatsStore {
    async fn start(&self) -> Result<()> {
        ensure_assets(self.jetstream(), self.asset_policy()).await
    }

    async fn stop(&self) -> Result<()> {
        Ok(())
    }

    async fn wipe_data(&self) -> Result<()> {
        Ok(())
    }

    async fn healthy(&self) -> bool {
        self.client().flush().await.is_ok()
    }
}
