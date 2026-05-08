use async_nats::HeaderMap;
use async_nats::header::{NATS_EXPECTED_STREAM, NATS_MESSAGE_ID};
use async_nats::jetstream;
use async_nats::jetstream::message::PublishMessage;
use ployz_types::error::{Error, Result};
use ployz_types::model::RoutingEvent;

use crate::NatsStore;
use crate::buckets::NatsAssetNames;
use crate::subjects::{self, NatsScope};

pub const PLOYZ_ROUTING_CAUSE: &str = "Ployz-Routing-Cause";
pub const PLOYZ_ROUTING_EVENT_ID: &str = "Ployz-Routing-Event-Id";

struct RoutingPublish {
    subject: String,
    stream: String,
    headers: HeaderMap,
    payload: Vec<u8>,
}

impl NatsStore {
    pub(crate) async fn publish_routing_events(
        &self,
        operation_id: impl AsRef<str>,
        cause: impl AsRef<str>,
        events: &[RoutingEvent],
    ) -> Result<()> {
        publish_routing_events_in(self.jetstream(), self.scope(), operation_id, cause, events).await
    }
}

pub(crate) async fn publish_routing_events_in(
    js: &jetstream::Context,
    scope: &NatsScope,
    operation_id: impl AsRef<str>,
    cause: impl AsRef<str>,
    events: &[RoutingEvent],
) -> Result<()> {
    for spec in routing_publishes_in(scope, operation_id.as_ref(), cause.as_ref(), events)? {
        let publish = PublishMessage::build()
            .payload(spec.payload.into())
            .headers(spec.headers)
            .expected_stream(spec.stream);
        let ack = js
            .send_publish(spec.subject, publish)
            .await
            .map_err(|error| Error::operation("nats_routing_publish", format!("{error:?}")))?;
        ack.await
            .map_err(|error| Error::operation("nats_routing_ack", format!("{error:?}")))?;
    }
    Ok(())
}

fn routing_publishes_in(
    scope: &NatsScope,
    operation_id: &str,
    cause: &str,
    events: &[RoutingEvent],
) -> Result<Vec<RoutingPublish>> {
    let stream = NatsAssetNames::new(scope).routing_events_stream;
    events
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let event_id = format!("{operation_id}:{}", index + 1);
            let mut headers = HeaderMap::new();
            headers.insert(NATS_EXPECTED_STREAM, stream.as_str());
            headers.insert(NATS_MESSAGE_ID, event_id.as_str());
            headers.insert(PLOYZ_ROUTING_EVENT_ID, event_id.as_str());
            headers.insert(PLOYZ_ROUTING_CAUSE, cause);
            let payload = serde_json::to_vec(event).map_err(|error| {
                Error::operation("nats_routing_event_encode", error.to_string())
            })?;
            Ok(RoutingPublish {
                subject: subjects::routing_event_in(scope, &event_id),
                stream: stream.clone(),
                headers,
                payload,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{AuthorityId, InstallationId};
    use ployz_types::model::{
        MachineId, MachineLifecycle, MachineMembership, MachineTopology, OverlayIp, PublicKey,
        StorageParticipation,
    };

    fn local_routing_publishes(
        operation_id: &str,
        cause: &str,
        events: &[RoutingEvent],
    ) -> Result<Vec<RoutingPublish>> {
        routing_publishes_in(&NatsScope::local_default(), operation_id, cause, events)
    }

    #[test]
    fn routing_event_specs_set_event_headers() {
        let events = vec![
            RoutingEvent::MachineUpsert(test_machine("machine-1")),
            RoutingEvent::MachineUpsert(test_machine("machine-2")),
        ];

        let specs =
            local_routing_publishes("machine:machine-1", "machine.upsert", &events).expect("specs");

        assert_eq!(specs.len(), 2);
        assert_eq!(
            header(&specs[0].headers, PLOYZ_ROUTING_EVENT_ID),
            "machine:machine-1:1"
        );
        assert_eq!(
            header(&specs[0].headers, "Nats-Msg-Id"),
            "machine:machine-1:1"
        );
        assert_eq!(
            header(&specs[0].headers, PLOYZ_ROUTING_CAUSE),
            "machine.upsert"
        );
        assert_eq!(
            header(&specs[0].headers, "Nats-Expected-Stream"),
            NatsAssetNames::new(&NatsScope::local_default()).routing_events_stream
        );
        assert_eq!(
            specs[0].stream,
            NatsAssetNames::new(&NatsScope::local_default()).routing_events_stream
        );
        assert_eq!(
            header(&specs[1].headers, PLOYZ_ROUTING_EVENT_ID),
            "machine:machine-1:2"
        );
    }

    #[test]
    fn routing_event_specs_use_scope_for_publish_subjects() {
        let scope = NatsScope::new(
            InstallationId("inst-acme".into()),
            AuthorityId("auth-sin".into()),
        );
        let specs = routing_publishes_in(
            &scope,
            "machine:machine-1",
            "machine.upsert",
            &[RoutingEvent::MachineUpsert(test_machine("machine-1"))],
        )
        .expect("build specs");

        assert_eq!(
            specs[0].subject,
            "ployz.v1.inst-acme.auth-sin.routing.event.machine%3Amachine-1%3A1"
        );
    }

    #[test]
    fn routing_event_specs_do_not_emulate_batch_commit_protocol() {
        let specs = local_routing_publishes(
            "deploy:deploy-1",
            "deploy.commit",
            &[RoutingEvent::MachineUpsert(test_machine("machine-1"))],
        )
        .expect("build specs");

        let headers = &specs[0].headers;
        assert!(headers.get(PLOYZ_ROUTING_EVENT_ID).is_some());
        assert!(headers.get("Nats-Batch-Id").is_none());
        assert!(headers.get("Nats-Batch-Sequence").is_none());
        assert!(headers.get("Nats-Batch-Commit").is_none());
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
            region_role: ployz_types::model::RegionRole::HomeData,
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle: MachineLifecycle::Active,
            storage: true,
            storage_participation: StorageParticipation::default_authority(),
            created_at: 1,
            updated_at: 1,
            labels: Default::default(),
        }
    }
}
