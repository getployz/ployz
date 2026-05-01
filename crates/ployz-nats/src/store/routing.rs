use async_nats::HeaderMap;
use async_nats::header::NATS_EXPECTED_STREAM;
use async_nats::jetstream;
use async_nats::jetstream::message::PublishMessage;
use ployz_types::error::{Error, Result};
use ployz_types::model::RoutingEvent;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::NatsStore;
use crate::subjects::{self, ROUTING_EVENTS_STREAM};

pub const NATS_BATCH_ID: &str = "Nats-Batch-Id";
pub const NATS_BATCH_SEQUENCE: &str = "Nats-Batch-Sequence";
pub const NATS_BATCH_COMMIT: &str = "Nats-Batch-Commit";
pub const PLOYZ_ROUTING_CAUSE: &str = "Ployz-Routing-Cause";
pub const PLOYZ_ROUTING_COUNT: &str = "Ployz-Routing-Count";
pub const PLOYZ_ROUTING_LOGICAL_BATCH_ID: &str = "Ployz-Routing-Logical-Batch-Id";
pub const PLOYZ_ROUTING_SEQUENCE: &str = "Ployz-Routing-Sequence";

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
    let Some((commit, staged)) = specs.split_last() else {
        return Ok(());
    };
    for spec in staged {
        publish_routing_batch_part(js, spec).await?;
    }
    let publish = PublishMessage::build()
        .payload(commit.payload.clone().into())
        .headers(commit.headers.clone())
        .expected_stream(ROUTING_EVENTS_STREAM);
    let ack = js
        .send_publish(commit.subject.clone(), publish)
        .await
        .map_err(|error| Error::operation("nats_routing_publish", format!("{error:?}")))?;
    ack.await
        .map_err(|error| Error::operation("nats_routing_ack", format!("{error:?}")))?;
    Ok(())
}

async fn publish_routing_batch_part(
    js: &jetstream::Context,
    spec: &RoutingPublishSpec,
) -> Result<()> {
    let response = js
        .client()
        .request_with_headers(
            spec.subject.clone(),
            spec.headers.clone(),
            spec.payload.clone().into(),
        )
        .await
        .map_err(|error| Error::operation("nats_routing_publish", format!("{error:?}")))?;
    if response.payload.is_empty() {
        return Ok(());
    }
    Err(Error::operation(
        "nats_routing_ack",
        format!(
            "unexpected batch part ack for '{}': {}",
            spec.subject,
            String::from_utf8_lossy(response.payload.as_ref())
        ),
    ))
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

    let atomic_batch_id = format!("{batch_id}:{}", unique_routing_publish_id());
    events
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let sequence = index + 1;
            let mut headers = HeaderMap::new();
            headers.insert(NATS_BATCH_ID, atomic_batch_id.as_str());
            headers.insert(NATS_BATCH_SEQUENCE, sequence.to_string());
            headers.insert(NATS_EXPECTED_STREAM, ROUTING_EVENTS_STREAM);
            headers.insert(PLOYZ_ROUTING_LOGICAL_BATCH_ID, batch_id);
            headers.insert(PLOYZ_ROUTING_CAUSE, cause);
            headers.insert(PLOYZ_ROUTING_COUNT, count.to_string());
            headers.insert(PLOYZ_ROUTING_SEQUENCE, sequence.to_string());
            if sequence == count {
                headers.insert(NATS_BATCH_COMMIT, "1");
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

fn unique_routing_publish_id() -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => format!("{:x}-{sequence:x}", duration.as_nanos()),
        Err(error) => format!("clock-error-{}-{sequence:x}", error.duration().as_nanos()),
    }
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
        assert!(header(&specs[0].headers, NATS_BATCH_ID).starts_with("batch-1:"));
        assert_eq!(
            header(&specs[0].headers, PLOYZ_ROUTING_LOGICAL_BATCH_ID),
            "batch-1"
        );
        assert_eq!(header(&specs[0].headers, NATS_BATCH_SEQUENCE), "1");
        assert_eq!(
            header(&specs[0].headers, "Nats-Expected-Stream"),
            ROUTING_EVENTS_STREAM
        );
        assert_eq!(header(&specs[0].headers, PLOYZ_ROUTING_SEQUENCE), "1");
        assert!(specs[0].headers.get(NATS_BATCH_COMMIT).is_none());
        assert_eq!(header(&specs[1].headers, NATS_BATCH_SEQUENCE), "2");
        assert_eq!(
            header(&specs[1].headers, "Nats-Expected-Stream"),
            ROUTING_EVENTS_STREAM
        );
        assert_eq!(header(&specs[1].headers, PLOYZ_ROUTING_SEQUENCE), "2");
        assert_eq!(header(&specs[1].headers, NATS_BATCH_COMMIT), "1");
        assert_eq!(header(&specs[1].headers, PLOYZ_ROUTING_COUNT), "2");
    }

    #[test]
    fn routing_batch_message_ids_change_between_publish_attempts() {
        let events = vec![RoutingEvent::MachineAdded(test_machine("machine-1"))];

        let first = routing_publish_specs("machine:machine-1", "machine.upsert", &events)
            .expect("first specs");
        let second = routing_publish_specs("machine:machine-1", "machine.upsert", &events)
            .expect("second specs");

        assert_ne!(
            header(&first[0].headers, NATS_BATCH_ID),
            header(&second[0].headers, NATS_BATCH_ID)
        );
        assert!(first[0].headers.get("Nats-Msg-Id").is_none());
        assert!(second[0].headers.get("Nats-Msg-Id").is_none());
    }

    #[test]
    fn atomic_routing_batches_do_not_use_unsupported_message_id_headers() {
        let specs = routing_publish_specs(
            "deploy:deploy-1",
            "deploy.commit",
            &[RoutingEvent::MachineAdded(test_machine("machine-1"))],
        )
        .expect("build specs");

        let headers = &specs[0].headers;
        assert!(headers.get(NATS_BATCH_ID).is_some());
        assert!(headers.get(NATS_BATCH_SEQUENCE).is_some());
        assert!(headers.get(NATS_BATCH_COMMIT).is_some());
        assert!(headers.get("Nats-Msg-Id").is_none());
        assert!(headers.get("Nats-Expected-Last-Msg-Id").is_none());
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
