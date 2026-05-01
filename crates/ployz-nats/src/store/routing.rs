use async_nats::HeaderMap;
use async_nats::jetstream;
use async_nats::jetstream::message::PublishMessage;
use ployz_types::error::{Error, Result};
use ployz_types::model::RoutingEvent;

use crate::NatsStore;
use crate::subjects::{self, ROUTING_EVENTS_STREAM};

pub const NATS_BATCH_ID: &str = "Nats-Batch-Id";
pub const NATS_BATCH_SEQUENCE: &str = "Nats-Batch-Sequence";
pub const NATS_BATCH_COMMIT: &str = "Nats-Batch-Commit";
pub const PLOYZ_ROUTING_CAUSE: &str = "Ployz-Routing-Cause";
pub const PLOYZ_ROUTING_COUNT: &str = "Ployz-Routing-Count";

pub(crate) struct RoutingPublishSpec {
    pub subject: String,
    pub headers: HeaderMap,
    pub payload: Vec<u8>,
}

impl NatsStore {
    pub(crate) async fn publish_routing_batch(
        &self,
        batch_id: impl AsRef<str>,
        cause: impl AsRef<str>,
        events: &[RoutingEvent],
    ) -> Result<()> {
        publish_routing_batch(self.jetstream(), batch_id, cause, events).await
    }
}

pub(crate) async fn publish_routing_batch(
    js: &jetstream::Context,
    batch_id: impl AsRef<str>,
    cause: impl AsRef<str>,
    events: &[RoutingEvent],
) -> Result<()> {
    let specs = routing_publish_specs(batch_id.as_ref(), cause.as_ref(), events)?;
    let mut ack_futures = Vec::with_capacity(specs.len());
    for spec in specs {
        let publish = PublishMessage::build()
            .payload(spec.payload.into())
            .headers(spec.headers)
            .expected_stream(ROUTING_EVENTS_STREAM);
        let ack = js
            .send_publish(spec.subject, publish)
            .await
            .map_err(|error| Error::operation("nats_routing_publish", format!("{error:?}")))?;
        ack_futures.push(ack);
    }
    for ack in ack_futures {
        ack.await
            .map_err(|error| Error::operation("nats_routing_ack", format!("{error:?}")))?;
    }
    Ok(())
}

pub(crate) fn routing_publish_specs(
    batch_id: &str,
    cause: &str,
    events: &[RoutingEvent],
) -> Result<Vec<RoutingPublishSpec>> {
    let count = events.len();
    if count == 0 {
        return Ok(Vec::new());
    }

    events
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let sequence = index + 1;
            let mut headers = HeaderMap::new();
            headers.insert(NATS_BATCH_ID, batch_id);
            headers.insert(NATS_BATCH_SEQUENCE, sequence.to_string());
            headers.insert("Nats-Msg-Id", format!("routing:{batch_id}:{sequence}"));
            headers.insert(PLOYZ_ROUTING_CAUSE, cause);
            headers.insert(PLOYZ_ROUTING_COUNT, count.to_string());
            if sequence == count {
                headers.insert(NATS_BATCH_COMMIT, count.to_string());
            }
            let payload = serde_json::to_vec(event).map_err(|error| {
                Error::operation("nats_routing_event_encode", error.to_string())
            })?;
            Ok(RoutingPublishSpec {
                subject: subjects::routing_event(batch_id, sequence),
                headers,
                payload,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{
        MachineId, MachineLifecycle, MachineMembership, MachineRole, MachineTopology, OverlayIp,
        PublicKey,
    };

    #[test]
    fn routing_batch_specs_set_atomic_headers_and_commit_marker() {
        let events = vec![
            RoutingEvent::MachineAdded(test_machine("machine-1")),
            RoutingEvent::MachineAdded(test_machine("machine-2")),
        ];

        let specs =
            routing_publish_specs("batch-1", "machine.upsert", &events).expect("build specs");

        assert_eq!(specs.len(), 2);
        assert_eq!(header(&specs[0].headers, NATS_BATCH_ID), "batch-1");
        assert_eq!(header(&specs[0].headers, NATS_BATCH_SEQUENCE), "1");
        assert_eq!(
            header(&specs[0].headers, "Nats-Msg-Id"),
            "routing:batch-1:1"
        );
        assert!(specs[0].headers.get(NATS_BATCH_COMMIT).is_none());
        assert_eq!(header(&specs[1].headers, NATS_BATCH_SEQUENCE), "2");
        assert_eq!(header(&specs[1].headers, NATS_BATCH_COMMIT), "2");
        assert_eq!(header(&specs[1].headers, PLOYZ_ROUTING_COUNT), "2");
    }

    fn header(headers: &HeaderMap, name: &str) -> String {
        headers
            .get(name)
            .map(|value| value.as_str().to_string())
            .unwrap_or_default()
    }

    fn test_machine(id: &str) -> MachineMembership {
        MachineMembership {
            id: MachineId(id.to_string()),
            public_key: PublicKey([0; 32]),
            overlay_ip: OverlayIp("fd00::1".parse().expect("valid overlay")),
            topology: MachineTopology::local(),
            control_target: None,
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle: MachineLifecycle::Active,
            role: MachineRole::StorageCandidate,
            created_at: 1,
            updated_at: 1,
            labels: Default::default(),
        }
    }
}
