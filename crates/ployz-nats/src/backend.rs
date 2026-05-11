use async_nats::jetstream::consumer::push;
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy};
use async_trait::async_trait;
use futures_util::StreamExt;
use ployz_store_api::{
    MachineMembershipStore, PeerRttStore, RoutingEventEnvelope, RoutingEventSubscription,
    RoutingStateStore, StoreRuntimeControl, SyncProbe, SyncStatus,
};
use ployz_types::error::{Error, Result};
use ployz_types::model::{RoutingEvent, RoutingState};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::warn;

use crate::NatsStore;
use crate::buckets::ensure_assets_in;
use crate::store::instances::list_all_instance_status;
use crate::store::kv_json;
use crate::store::routing::{PLOYZ_ROUTING_CAUSE, PLOYZ_ROUTING_EVENT_ID};
use crate::subjects::NatsScope;

const ROUTING_CONSUMER_CHANNEL_CAPACITY: usize = 128;
const ROUTING_CONSUMER_ACK_WAIT: Duration = Duration::from_secs(30);
const ROUTING_CONSUMER_IDLE_HEARTBEAT: Duration = Duration::from_secs(5);
const ROUTING_EPHEMERAL_INACTIVE_THRESHOLD: Duration = Duration::from_secs(60);

#[async_trait]
impl RoutingStateStore for NatsStore {
    async fn load_routing_state(&self) -> Result<RoutingState> {
        let facts = self.deploy_commit_facts().await?;
        Ok(RoutingState {
            machines: MachineMembershipStore::list_machines(self).await?,
            revisions: facts.all_revisions(),
            releases: facts.all_releases(),
            instances: list_all_instance_status(self).await?,
        })
    }

    async fn subscribe_routing_events(&self) -> Result<RoutingEventSubscription> {
        let stream = self
            .jetstream()
            .get_stream(self.assets().routing_events_stream.as_str())
            .await
            .map_err(|error| Error::operation("nats_routing_stream", format!("{error:?}")))?;
        let mut stream = stream;
        let start_sequence = routing_subscription_start_sequence(&mut stream).await?;
        let state = RoutingStateStore::load_routing_state(self).await?;
        let consumer: async_nats::jetstream::consumer::PushConsumer = stream
            .create_consumer(routing_consumer_config(
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
    start_sequence: u64,
    deliver_subject: String,
    scope: &NatsScope,
) -> push::Config {
    push::Config {
        deliver_subject,
        deliver_policy: DeliverPolicy::ByStartSequence { start_sequence },
        ack_policy: AckPolicy::Explicit,
        ack_wait: ROUTING_CONSUMER_ACK_WAIT,
        idle_heartbeat: ROUTING_CONSUMER_IDLE_HEARTBEAT,
        max_ack_pending: ROUTING_CONSUMER_CHANNEL_CAPACITY as i64,
        filter_subject: crate::subjects::routing_event_filter_in(scope),
        description: Some(String::from("temporary ployz routing subscription")),
        memory_storage: true,
        inactive_threshold: ROUTING_EPHEMERAL_INACTIVE_THRESHOLD,
        ..Default::default()
    }
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

#[async_trait]
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

#[async_trait]
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
    use ployz_types::model::{MachineId, MachineMembership};

    #[test]
    fn routing_consumers_are_ephemeral() {
        let config = routing_consumer_config(
            42,
            "_INBOX.runtime.1".to_string(),
            &NatsScope::local_default(),
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
    fn routing_consumer_filter_uses_scope() {
        let scope = NatsScope::new(
            ployz_types::model::InstallationId::new("inst-acme"),
            ployz_types::model::AuthorityId::new("auth-sin"),
        );
        let config = routing_consumer_config(1, "_INBOX.runtime.1".to_string(), &scope);

        assert_eq!(
            config.filter_subject,
            "ployz.v1.inst-acme.auth-sin.routing.event.>"
        );
    }

    #[test]
    fn routing_replayed_events_preserve_recreate_already_in_snapshot() {
        let removed = test_machine("machine-1");
        let mut recreated = test_machine("machine-1");
        recreated.updated_at = 2;
        let mut state = RoutingState {
            machines: vec![recreated.clone()],
            revisions: Vec::new(),
            releases: Vec::new(),
            instances: Vec::new(),
        };

        ployz_store_api::apply_routing_events(
            &mut state,
            [
                RoutingEvent::MachineRemoved { id: removed.id },
                RoutingEvent::MachineUpsert(recreated.clone()),
            ],
        );

        assert_eq!(state.machines, vec![recreated]);
    }

    #[test]
    fn routing_event_decode_preserves_event_metadata() {
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(PLOYZ_ROUTING_EVENT_ID, "deploy:deploy-1:1");
        headers.insert(PLOYZ_ROUTING_CAUSE, "deploy.commit");
        let event = RoutingEvent::MachineUpsert(test_machine("machine-1"));
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
        let event = RoutingEvent::MachineUpsert(test_machine("machine-1"));
        let payload = serde_json::to_vec(&event).expect("routing event should encode");

        let error = decode_routing_event(Some(&headers), &payload)
            .expect_err("missing event id should be visible to subscriber");

        assert!(error.to_string().contains("nats_routing_headers"));
        assert!(error.to_string().contains(PLOYZ_ROUTING_EVENT_ID));
    }

    #[test]
    fn routing_event_decode_rejects_missing_headers() {
        let event = RoutingEvent::MachineUpsert(test_machine("machine-1"));
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
            id: MachineId::new(id.to_string()),
            public_key: ployz_types::model::PublicKey([0; 32]),
            overlay_ip: ployz_types::model::OverlayIp("fd00::1".parse().expect("valid overlay")),
            topology: ployz_types::model::MachineTopology::local(),
            region_role: ployz_types::model::RegionRole::HomeData,
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle: ployz_types::model::MachineLifecycle::Active,
            storage_role: ployz_types::model::StorageParticipation::default_authority().into(),
            created_at: 1,
            updated_at: 1,
            labels: Default::default(),
        }
    }
}
