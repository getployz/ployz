use async_nats::jetstream::ErrorCode;
use async_nats::jetstream::Message;
use async_nats::jetstream::consumer::push;
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy};
use async_nats::jetstream::stream::ConsumerErrorKind;
use async_trait::async_trait;
use futures_util::StreamExt;
use ployz_store_api::{
    AcmeChallengeSubscription, CertificateStore, CertificateSubscription, DeployCommit,
    DeployRecordUpdate, DeployRepository, DeployRevisionUpsert, DeploySnapshot,
    InstanceStatusRepository, InviteRepository, MachineRegistry, MachineSubscription,
    PeerRttObservation, PeerRttStore, RoutingBatchSubscription, RoutingEventBatch,
    RoutingSnapshotReader, RoutingSubscription, StoreBackend, StoreRuntimeControl, SyncProbe,
    SyncStatus,
};
use ployz_types::error::{Error, Result};
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeReadinessRecord, AcmeChallengeRecord, CertificateRecord,
    DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineId,
    MachineMembership, RoutingEvent, RoutingState, ServiceReleaseRecord, VolumeRecord,
};
use ployz_types::spec::Namespace;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::warn;

use crate::NatsStore;
use crate::buckets::ensure_assets;
use crate::store::instances::list_all_instance_status;
use crate::store::kv_json;
use crate::store::routing::{
    NATS_BATCH_COMMIT, NATS_BATCH_ID, NATS_BATCH_SEQUENCE, PLOYZ_ROUTING_CAUSE,
    PLOYZ_ROUTING_COUNT, PLOYZ_ROUTING_SEQUENCE,
};
use crate::subjects::ROUTING_EVENTS_STREAM;

const ROUTING_CONSUMER_CHANNEL_CAPACITY: usize = 128;
const ROUTING_CONSUMER_ACK_WAIT: Duration = Duration::from_secs(30);
const ROUTING_CONSUMER_IDLE_HEARTBEAT: Duration = Duration::from_secs(5);
const ROUTING_EPHEMERAL_INACTIVE_THRESHOLD: Duration = Duration::from_secs(60);

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

    async fn subscribe_routing_batches(
        &self,
        subscription: RoutingSubscription,
    ) -> Result<RoutingBatchSubscription> {
        RoutingSnapshotReader::subscribe_routing_batches(self, subscription).await
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

    async fn upsert_acme_challenge_readiness(
        &self,
        record: &AcmeChallengeReadinessRecord,
    ) -> Result<()> {
        CertificateStore::upsert_acme_challenge_readiness(self, record).await
    }

    async fn list_acme_challenge_readiness(
        &self,
        hostname: &str,
        token: &str,
    ) -> Result<Vec<AcmeChallengeReadinessRecord>> {
        CertificateStore::list_acme_challenge_readiness(self, hostname, token).await
    }

    async fn sync_status(&self) -> Result<SyncStatus> {
        SyncProbe::sync_status(self).await
    }

    async fn peer_rtt_observations(&self) -> Result<Vec<PeerRttObservation>> {
        PeerRttStore::peer_rtt_observations(self).await
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

    async fn subscribe_routing_batches(
        &self,
        subscription: RoutingSubscription,
    ) -> Result<RoutingBatchSubscription> {
        let consumer_id = subscription.consumer_id().to_string();
        let consumer_name = routing_consumer_name(&consumer_id);
        let temporary = subscription.is_temporary();
        let stream = self
            .jetstream()
            .get_stream(ROUTING_EVENTS_STREAM)
            .await
            .map_err(|error| Error::operation("nats_routing_stream", format!("{error:?}")))?;
        let mut stream = stream;
        let start_sequence = routing_subscription_start_sequence(&mut stream).await?;
        let state = RoutingSnapshotReader::load_routing_state(self).await?;
        if !temporary {
            delete_existing_routing_consumer(&stream, &consumer_name).await?;
        }
        let consumer: async_nats::jetstream::consumer::PushConsumer = stream
            .create_consumer(routing_consumer_config(
                &subscription,
                &consumer_name,
                start_sequence,
                self.client().new_inbox(),
            ))
            .await
            .map_err(|error| Error::operation("nats_routing_consumer", format!("{error:?}")))?;
        let mut messages = consumer
            .messages()
            .await
            .map_err(|error| Error::operation("nats_routing_messages", format!("{error:?}")))?;
        let (tx, rx) = mpsc::channel(ROUTING_CONSUMER_CHANNEL_CAPACITY);
        tokio::spawn(async move {
            let mut pending = PendingRoutingBatches::default();
            loop {
                let next = tokio::select! {
                    _ = tx.closed() => break,
                    next = messages.next() => next,
                };
                let Some(next) = next else {
                    break;
                };
                let message = match next {
                    Ok(message) => message,
                    Err(error) => {
                        let error = Error::operation("nats_routing_consumer", format!("{error:?}"));
                        warn!(?error, "NATS routing batch consumer failed");
                        let _ = tx.send(Err(error)).await;
                        break;
                    }
                };
                match pending.push(message) {
                    Ok(None) => {}
                    Ok(Some(complete)) => {
                        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
                        let batch = RoutingEventBatch::with_ack(
                            complete.batch_id,
                            complete.cause,
                            complete.events,
                            ack_tx,
                        );
                        if tx.send(Ok(batch)).await.is_err() {
                            break;
                        }
                        if ack_rx.await.is_ok() {
                            let mut ack_error = None;
                            for message in complete.messages {
                                if let Err(error) = message.ack().await {
                                    warn!(?error, "NATS routing batch message ack failed");
                                    ack_error.get_or_insert_with(|| {
                                        Error::operation(
                                            "nats_routing_ack",
                                            format!("routing batch message ack failed: {error:?}"),
                                        )
                                    });
                                }
                            }
                            if let Some(error) = ack_error {
                                let _ = tx.send(Err(error)).await;
                                break;
                            }
                        }
                    }
                    Err(error) => {
                        warn!(?error, "NATS routing batch decode failed");
                        let _ = tx.send(Err(error)).await;
                        break;
                    }
                }
            }
        });
        Ok((state, rx))
    }
}

fn routing_consumer_config(
    subscription: &RoutingSubscription,
    consumer_name: &str,
    start_sequence: u64,
    deliver_subject: String,
) -> push::Config {
    let mut config = push::Config {
        deliver_subject,
        deliver_policy: DeliverPolicy::ByStartSequence { start_sequence },
        ack_policy: AckPolicy::Explicit,
        ack_wait: ROUTING_CONSUMER_ACK_WAIT,
        idle_heartbeat: ROUTING_CONSUMER_IDLE_HEARTBEAT,
        max_ack_pending: ROUTING_CONSUMER_CHANNEL_CAPACITY as i64,
        filter_subject: "routing.events.>".to_string(),
        ..Default::default()
    };
    match subscription {
        RoutingSubscription::Durable { .. } => {
            config.durable_name = Some(consumer_name.to_string());
            config.name = Some(consumer_name.to_string());
        }
        RoutingSubscription::Temporary { consumer_id } => {
            config.description = Some(format!(
                "temporary ployz routing subscription {consumer_id}"
            ));
            config.memory_storage = true;
            config.inactive_threshold = ROUTING_EPHEMERAL_INACTIVE_THRESHOLD;
        }
    }
    config
}

async fn routing_subscription_start_sequence(
    stream: &mut async_nats::jetstream::stream::Stream,
) -> Result<u64> {
    stream
        .info()
        .await
        .map(|info| kv_json::next_sequence(info.state.last_sequence))
        .map_err(|error| Error::operation("nats_routing_stream_info", format!("{error:?}")))
}

async fn delete_existing_routing_consumer(
    stream: &async_nats::jetstream::stream::Stream,
    consumer_name: &str,
) -> Result<()> {
    match stream.delete_consumer(consumer_name).await {
        Ok(_) => Ok(()),
        Err(error) if is_missing_consumer(&error) => Ok(()),
        Err(error) => Err(Error::operation(
            "nats_routing_consumer_delete",
            format!("{error:?}"),
        )),
    }
}

fn is_missing_consumer(error: &async_nats::jetstream::stream::ConsumerError) -> bool {
    match error.kind() {
        ConsumerErrorKind::JetStream(error) => {
            error.error_code() == ErrorCode::CONSUMER_DOES_NOT_EXIST
                || error.error_code() == ErrorCode::CONSUMER_NOT_FOUND
        }
        ConsumerErrorKind::TimedOut
        | ConsumerErrorKind::Request
        | ConsumerErrorKind::InvalidConsumerType
        | ConsumerErrorKind::InvalidName
        | ConsumerErrorKind::Other => false,
    }
}

#[derive(Default)]
struct PendingRoutingBatches {
    by_id: HashMap<String, PendingRoutingBatch>,
}

#[derive(Default)]
struct PendingRoutingBatch {
    cause: Option<String>,
    messages: Vec<Message>,
    events: Vec<RoutingEvent>,
}

struct CompleteRoutingBatch {
    batch_id: String,
    cause: Option<String>,
    messages: Vec<Message>,
    events: Vec<RoutingEvent>,
}

impl PendingRoutingBatches {
    fn push(&mut self, message: Message) -> Result<Option<CompleteRoutingBatch>> {
        let headers = message.headers.as_ref().ok_or_else(|| {
            Error::operation("nats_routing_headers", "routing event missing headers")
        })?;
        let batch_id = header(headers, NATS_BATCH_ID)?.to_string();
        let sequence = routing_sequence(headers)?;
        let batch = self.by_id.entry(batch_id.clone()).or_default();
        if sequence == 1 && !batch.events.is_empty() {
            *batch = PendingRoutingBatch::default();
        }
        let complete = batch.push(batch_id.clone(), sequence, message)?;
        if complete.is_some() {
            self.by_id.remove(&batch_id);
        }
        Ok(complete)
    }
}

impl PendingRoutingBatch {
    fn push(
        &mut self,
        batch_id: String,
        sequence: usize,
        message: Message,
    ) -> Result<Option<CompleteRoutingBatch>> {
        let headers = message.headers.as_ref().ok_or_else(|| {
            Error::operation("nats_routing_headers", "routing event missing headers")
        })?;
        if sequence != self.events.len() + 1 {
            return Err(Error::operation(
                "nats_routing_sequence",
                format!(
                    "batch '{batch_id}' sequence {sequence} did not follow {}",
                    self.events.len()
                ),
            ));
        }
        if self.events.is_empty() {
            self.cause = headers
                .get(PLOYZ_ROUTING_CAUSE)
                .map(|value| value.as_str().to_string());
        }
        let event = serde_json::from_slice::<RoutingEvent>(message.payload.as_ref())
            .map_err(|error| Error::operation("nats_routing_event_decode", error.to_string()))?;
        let commit = headers
            .get(NATS_BATCH_COMMIT)
            .map(|value| {
                value
                    .as_str()
                    .parse::<usize>()
                    .map_err(|error| Error::operation("nats_routing_commit", error.to_string()))
            })
            .transpose()?;
        let count = if commit.is_some() {
            Some(
                header(headers, PLOYZ_ROUTING_COUNT)?
                    .parse::<usize>()
                    .map_err(|error| Error::operation("nats_routing_count", error.to_string()))?,
            )
        } else {
            None
        };
        self.events.push(event);
        self.messages.push(message);
        if commit.is_none() {
            return Ok(None);
        };
        let Some(count) = count else {
            return Ok(None);
        };
        if count != self.events.len() {
            return Err(Error::operation(
                "nats_routing_commit",
                format!(
                    "batch '{batch_id}' commit count {count} did not match {} messages",
                    self.events.len()
                ),
            ));
        }
        Ok(Some(CompleteRoutingBatch {
            batch_id,
            cause: self.cause.take(),
            messages: std::mem::take(&mut self.messages),
            events: std::mem::take(&mut self.events),
        }))
    }
}

fn header<'a>(headers: &'a async_nats::HeaderMap, name: &str) -> Result<&'a str> {
    headers
        .get(name)
        .map(|value| value.as_str())
        .ok_or_else(|| Error::operation("nats_routing_headers", format!("missing {name} header")))
}

fn routing_sequence(headers: &async_nats::HeaderMap) -> Result<usize> {
    let value = headers
        .get(PLOYZ_ROUTING_SEQUENCE)
        .or_else(|| headers.get(NATS_BATCH_SEQUENCE))
        .map(|value| value.as_str())
        .ok_or_else(|| {
            Error::operation(
                "nats_routing_headers",
                format!("missing {PLOYZ_ROUTING_SEQUENCE} header"),
            )
        })?;
    value
        .parse::<usize>()
        .map_err(|error| Error::operation("nats_routing_sequence", error.to_string()))
}

fn routing_consumer_name(consumer_id: &str) -> String {
    crate::subjects::subject_token(consumer_id)
}

impl SyncProbe for NatsStore {
    async fn sync_status(&self) -> Result<SyncStatus> {
        match self.client().connection_state() {
            async_nats::connection::State::Connected => {
                if self.client().flush().await.is_ok() {
                    Ok(SyncStatus::Synced)
                } else {
                    Ok(SyncStatus::Disconnected)
                }
            }
            async_nats::connection::State::Pending => Ok(SyncStatus::Disconnected),
            async_nats::connection::State::Disconnected => Ok(SyncStatus::Disconnected),
        }
    }
}

impl PeerRttStore for NatsStore {}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_consumer_names_escape_subject_separators() {
        assert_eq!(
            routing_consumer_name("gateway.founder"),
            "gateway%2Efounder"
        );
        assert_eq!(
            routing_consumer_name("dns.machine/one"),
            "dns%2Emachine%2Fone"
        );
    }

    #[test]
    fn runtime_routing_consumers_are_temporary() {
        assert!(RoutingSubscription::temporary("ployzd.runtime.founder.1").is_temporary());
        assert!(!RoutingSubscription::durable("gateway.founder").is_temporary());
        assert!(!RoutingSubscription::durable("dns.founder").is_temporary());
    }

    #[test]
    fn temporary_routing_consumers_are_ephemeral() {
        let subscription = RoutingSubscription::temporary("ployzd.runtime.founder.1");
        let config =
            routing_consumer_config(&subscription, "unused", 42, "_INBOX.runtime.1".to_string());

        assert_eq!(config.durable_name, None);
        assert_eq!(config.name, None);
        assert_eq!(
            config.deliver_policy,
            DeliverPolicy::ByStartSequence { start_sequence: 42 }
        );
        assert!(config.memory_storage);
        assert_eq!(
            config.inactive_threshold,
            ROUTING_EPHEMERAL_INACTIVE_THRESHOLD
        );
    }

    #[test]
    fn durable_routing_consumers_are_named() {
        let subscription = RoutingSubscription::durable("gateway.founder");
        let consumer_name = routing_consumer_name(subscription.consumer_id());
        let config = routing_consumer_config(
            &subscription,
            &consumer_name,
            99,
            "_INBOX.gateway.1".to_string(),
        );

        assert_eq!(config.durable_name.as_deref(), Some("gateway%2Efounder"));
        assert_eq!(config.name.as_deref(), Some("gateway%2Efounder"));
        assert_eq!(
            config.deliver_policy,
            DeliverPolicy::ByStartSequence { start_sequence: 99 }
        );
        assert!(!config.memory_storage);
        assert_eq!(config.inactive_threshold, Duration::ZERO);
    }

    #[test]
    fn consumer_not_found_errors_are_idempotent_delete_success() {
        let error: async_nats::jetstream::Error = serde_json::from_value(serde_json::json!({
            "code": 404,
            "err_code": ErrorCode::CONSUMER_NOT_FOUND.0,
            "description": "consumer not found"
        }))
        .expect("valid JetStream error");
        let error =
            async_nats::jetstream::stream::ConsumerError::new(ConsumerErrorKind::JetStream(error));

        assert!(is_missing_consumer(&error));
    }
}
