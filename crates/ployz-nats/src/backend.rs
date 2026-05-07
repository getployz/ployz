use async_nats::jetstream::ErrorCode;
use async_nats::jetstream::consumer::push;
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy};
use async_nats::jetstream::stream::ConsumerErrorKind;
use async_trait::async_trait;
use futures_util::StreamExt;
use ployz_store_api::{
    AcmeChallengeSubscription, CertificateStore, CertificateSubscription, DeployCommit,
    DeployRecordUpdate, DeployRepository, DeploySnapshot, InstanceStatusRepository,
    InviteRepository, MachineRegistry, MachineSubscription, PeerRttObservation, PeerRttStore,
    RoutingEventEnvelope, RoutingEventSubscription, RoutingSnapshotReader, RoutingSubscription,
    StoreBackend, StoreRuntimeControl, SyncProbe, SyncStatus,
};
use ployz_types::error::{Error, Result};
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeReadinessRecord, AcmeChallengeRecord, CertificateRecord,
    DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineId,
    MachineMembership, RoutingEvent, RoutingState, ServiceReleaseRecord, VolumeRecord,
};
use ployz_types::spec::Namespace;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::warn;

use crate::NatsStore;
use crate::buckets::ensure_assets_in;
use crate::store::instances::list_all_instance_status;
use crate::store::kv_json;
use crate::store::routing::{PLOYZ_ROUTING_CAUSE, PLOYZ_ROUTING_EVENT_ID};
use crate::subjects::{NatsScope, ROUTE_JOURNAL_STREAM};

const ROUTING_CONSUMER_CHANNEL_CAPACITY: usize = 128;
const ROUTING_CONSUMER_ACK_WAIT: Duration = Duration::from_secs(30);
const ROUTING_CONSUMER_IDLE_HEARTBEAT: Duration = Duration::from_secs(5);
const ROUTING_EPHEMERAL_INACTIVE_THRESHOLD: Duration = Duration::from_secs(60);

#[async_trait]
impl StoreBackend for NatsStore {
    async fn init(&self) -> Result<()> {
        ensure_assets_in(self.jetstream(), self.scope(), self.asset_policy()).await
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

    async fn subscribe_routing_events(
        &self,
        subscription: RoutingSubscription,
    ) -> Result<RoutingEventSubscription> {
        RoutingSnapshotReader::subscribe_routing_events(self, subscription).await
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

    async fn subscribe_routing_events(
        &self,
        subscription: RoutingSubscription,
    ) -> Result<RoutingEventSubscription> {
        let durable_consumer_name = subscription
            .durable_consumer_id()
            .map(routing_consumer_name);
        let stream = self
            .jetstream()
            .get_stream(ROUTE_JOURNAL_STREAM)
            .await
            .map_err(|error| Error::operation("nats_routing_stream", format!("{error:?}")))?;
        let mut stream = stream;
        let start_sequence = routing_subscription_start_sequence(&mut stream).await?;
        let state = RoutingSnapshotReader::load_routing_state(self).await?;
        if let Some(consumer_name) = durable_consumer_name.as_deref() {
            delete_existing_routing_consumer(&stream, consumer_name).await?;
        }
        let consumer: async_nats::jetstream::consumer::PushConsumer = stream
            .create_consumer(routing_consumer_config(
                &subscription,
                start_sequence,
                self.client().new_inbox(),
                self.scope(),
            ))
            .await
            .map_err(|error| Error::operation("nats_routing_consumer", format!("{error:?}")))?;
        let mut messages = consumer
            .messages()
            .await
            .map_err(|error| Error::operation("nats_routing_messages", format!("{error:?}")))?;
        let (tx, rx) = mpsc::channel(ROUTING_CONSUMER_CHANNEL_CAPACITY);
        tokio::spawn(async move {
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
                        warn!(?error, "NATS routing event consumer failed");
                        let _ = tx.send(Err(error)).await;
                        break;
                    }
                };
                match routing_event_envelope(message) {
                    Ok((message, event_id, cause, event)) => {
                        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
                        let envelope =
                            RoutingEventEnvelope::with_ack(event_id, cause, event, ack_tx);
                        if tx.send(Ok(envelope)).await.is_err() {
                            break;
                        }
                        if ack_rx.await.is_ok() {
                            if let Err(error) = message.ack().await {
                                warn!(?error, "NATS routing event message ack failed");
                                let error = Error::operation(
                                    "nats_routing_ack",
                                    format!("routing event message ack failed: {error:?}"),
                                );
                                let _ = tx.send(Err(error)).await;
                                break;
                            }
                        }
                    }
                    Err(error) => {
                        warn!(?error, "NATS routing event decode failed");
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
    start_sequence: u64,
    deliver_subject: String,
    scope: &NatsScope,
) -> push::Config {
    let mut config = push::Config {
        deliver_subject,
        deliver_policy: DeliverPolicy::ByStartSequence { start_sequence },
        ack_policy: AckPolicy::Explicit,
        ack_wait: ROUTING_CONSUMER_ACK_WAIT,
        idle_heartbeat: ROUTING_CONSUMER_IDLE_HEARTBEAT,
        max_ack_pending: ROUTING_CONSUMER_CHANNEL_CAPACITY as i64,
        filter_subject: crate::subjects::route_journal_filter_in(scope),
        ..Default::default()
    };
    match subscription {
        RoutingSubscription::Durable { consumer_id } => {
            let consumer_name = routing_consumer_name(consumer_id);
            config.durable_name = Some(consumer_name.to_string());
            config.name = Some(consumer_name);
        }
        RoutingSubscription::Temporary => {
            config.description = Some(String::from("temporary ployz routing subscription"));
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

fn routing_event_envelope(
    message: async_nats::jetstream::Message,
) -> Result<(
    async_nats::jetstream::Message,
    String,
    Option<String>,
    RoutingEvent,
)> {
    let (event_id, cause, event) =
        decode_routing_event(message.headers.as_ref(), message.payload.as_ref())?;
    Ok((message, event_id, cause, event))
}

fn decode_routing_event(
    headers: Option<&async_nats::HeaderMap>,
    payload: &[u8],
) -> Result<(String, Option<String>, RoutingEvent)> {
    let headers = headers
        .ok_or_else(|| Error::operation("nats_routing_headers", "routing event missing headers"))?;
    let event_id = header(headers, PLOYZ_ROUTING_EVENT_ID)?.to_string();
    let cause = headers
        .get(PLOYZ_ROUTING_CAUSE)
        .map(|value| value.as_str().to_string());
    let event = serde_json::from_slice::<RoutingEvent>(payload)
        .map_err(|error| Error::operation("nats_routing_event_decode", error.to_string()))?;
    Ok((event_id, cause, event))
}

fn header<'a>(headers: &'a async_nats::HeaderMap, name: &str) -> Result<&'a str> {
    headers
        .get(name)
        .map(|value| value.as_str())
        .ok_or_else(|| Error::operation("nats_routing_headers", format!("missing {name} header")))
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
        ensure_assets_in(self.jetstream(), self.scope(), self.asset_policy()).await
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
        assert!(RoutingSubscription::temporary().is_temporary());
        assert!(!RoutingSubscription::durable("gateway.founder").is_temporary());
        assert!(!RoutingSubscription::durable("dns.founder").is_temporary());
    }

    #[test]
    fn temporary_routing_consumers_are_ephemeral() {
        let subscription = RoutingSubscription::temporary();
        let config = routing_consumer_config(
            &subscription,
            42,
            "_INBOX.runtime.1".to_string(),
            &NatsScope::default(),
        );

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
        let config = routing_consumer_config(
            &subscription,
            99,
            "_INBOX.gateway.1".to_string(),
            &NatsScope::default(),
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
    fn routing_consumer_filter_uses_scope() {
        let scope = NatsScope::new(
            ployz_types::model::InstallationId("inst-acme".into()),
            ployz_types::model::AuthorityId("auth-sin".into()),
        );
        let subscription = RoutingSubscription::temporary();
        let config =
            routing_consumer_config(&subscription, 1, "_INBOX.runtime.1".to_string(), &scope);

        assert_eq!(
            config.filter_subject,
            "ployz.v1.inst-acme.auth-sin.route.journal.>"
        );
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

    #[test]
    fn routing_event_decode_preserves_event_metadata() {
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(PLOYZ_ROUTING_EVENT_ID, "deploy:deploy-1:1");
        headers.insert(PLOYZ_ROUTING_CAUSE, "deploy.commit");
        let event = RoutingEvent::MachineAdded(test_machine("machine-1"));
        let payload = serde_json::to_vec(&event).expect("routing event should encode");

        let (event_id, cause, decoded) =
            decode_routing_event(Some(&headers), &payload).expect("routing event should decode");

        assert_eq!(event_id, "deploy:deploy-1:1");
        assert_eq!(cause.as_deref(), Some("deploy.commit"));
        assert_eq!(decoded, event);
    }

    #[test]
    fn routing_event_decode_requires_event_id_header() {
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(PLOYZ_ROUTING_CAUSE, "deploy.commit");
        let event = RoutingEvent::MachineAdded(test_machine("machine-1"));
        let payload = serde_json::to_vec(&event).expect("routing event should encode");

        let error = decode_routing_event(Some(&headers), &payload)
            .expect_err("missing event id should be visible to subscriber");

        assert!(error.to_string().contains("nats_routing_headers"));
        assert!(error.to_string().contains(PLOYZ_ROUTING_EVENT_ID));
    }

    #[test]
    fn routing_event_decode_rejects_missing_headers() {
        let event = RoutingEvent::MachineAdded(test_machine("machine-1"));
        let payload = serde_json::to_vec(&event).expect("routing event should encode");

        let error = decode_routing_event(None, &payload)
            .expect_err("missing headers should be visible to subscriber");

        assert!(error.to_string().contains("nats_routing_headers"));
        assert!(error.to_string().contains("missing headers"));
    }

    #[test]
    fn routing_event_decode_rejects_malformed_payload() {
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(PLOYZ_ROUTING_EVENT_ID, "deploy:deploy-1:1");

        let error = decode_routing_event(Some(&headers), b"not json")
            .expect_err("malformed routing payload should be visible to subscriber");

        assert!(error.to_string().contains("nats_routing_event_decode"));
    }

    fn test_machine(id: &str) -> MachineMembership {
        MachineMembership {
            id: MachineId(id.to_string()),
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
            labels: Default::default(),
        }
    }
}
