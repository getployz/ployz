use std::time::Duration;

use ployz_api::{RuntimeWatchFrame, runtime_frame_from_event, sort_routing_state};
use ployz_store_api::{RoutingEventSubscriptionUpdate, RoutingSnapshotReader};
use ployz_types::model::{RoutingEvent, RoutingState};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::daemon::DaemonState;

type RuntimeEventUpdate = Result<RoutingEvent, String>;

impl DaemonState {
    pub async fn open_runtime_subscription(
        &self,
    ) -> Result<(RoutingState, mpsc::Receiver<RuntimeEventUpdate>), Box<ployz_api::DaemonResponse>>
    {
        let active = self.require_active("NO_MESH", "no mesh is running")?;
        let (state, mut envelopes) = active
            .mesh
            .store
            .subscribe_routing_events()
            .await
            .map_err(|error| Box::new(self.err("RUNTIME_SUBSCRIBE_FAILED", error.to_string())))?;
        let (tx, rx) = mpsc::channel(1024);
        tokio::spawn(async move { relay_runtime_events(&mut envelopes, tx).await });
        Ok((state, rx))
    }
}

async fn relay_runtime_events(
    events: &mut mpsc::Receiver<RoutingEventSubscriptionUpdate>,
    tx: mpsc::Sender<RuntimeEventUpdate>,
) {
    while let Some(envelope) = events.recv().await {
        let envelope = match envelope {
            Ok(envelope) => envelope,
            Err(error) => {
                warn!(%error, "runtime routing event relay failed");
                let _ = tx.send(Err(error.to_string())).await;
                return;
            }
        };
        if tx.send(Ok(envelope.event.clone())).await.is_err() {
            let _ = envelope.ack().await;
            return;
        }
        if let Err(error) = envelope.ack().await {
            warn!(%error, "runtime routing event ack failed");
            match tx.try_send(Err(error.to_string())) {
                Ok(()) | Err(TrySendError::Closed(_)) => {}
                Err(TrySendError::Full(_)) => {
                    warn!("runtime routing event channel full after ack failure");
                }
            }
            return;
        }
    }
}

pub async fn stream_runtime_frames(
    mut initial: RoutingState,
    mut events: mpsc::Receiver<RuntimeEventUpdate>,
    frames: mpsc::Sender<RuntimeWatchFrame>,
    cancel: CancellationToken,
) {
    sort_routing_state(&mut initial);
    if frames
        .send(RuntimeWatchFrame::Snapshot { state: initial })
        .await
        .is_err()
    {
        return;
    }

    let heartbeat_period = Duration::from_secs(25);
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + heartbeat_period,
        heartbeat_period,
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = frames.closed() => return,
            _ = heartbeat.tick() => {
                if frames.send(RuntimeWatchFrame::Heartbeat).await.is_err() {
                    return;
                }
            }
            event = events.recv() => {
                let Some(event) = event else {
                    return;
                };
                let frame = match event {
                    Ok(event) => runtime_frame_from_event(event),
                    Err(message) => RuntimeWatchFrame::Error {
                        code: String::from("RUNTIME_SUBSCRIPTION_FAILED"),
                        message,
                    },
                };
                if frames.send(frame).await.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{relay_runtime_events, stream_runtime_frames};
    use ployz_api::{RuntimeCollection, RuntimeRecord, RuntimeWatchFrame};
    use ployz_store_api::RoutingEventEnvelope;
    use ployz_types::model::{
        DeployId, DrainState, InstanceId, InstancePhase, InstanceStatusRecord, MachineId,
        RoutingEvent, RoutingState, SlotId,
    };
    use ployz_types::spec::Namespace;
    use std::collections::BTreeMap;
    use std::net::Ipv4Addr;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn stream_runtime_frames_emits_sorted_snapshot_first() {
        let initial = RoutingState {
            machines: Vec::new(),
            revisions: Vec::new(),
            releases: Vec::new(),
            instances: vec![
                instance_record("instance-b", "prod", "web"),
                instance_record("instance-a", "prod", "api"),
            ],
        };
        let (event_tx, event_rx) = mpsc::channel(1);
        event_tx
            .send(Ok(RoutingEvent::InstanceAdded(instance_record(
                "instance-c",
                "prod",
                "worker",
            ))))
            .await
            .expect("queue event");
        drop(event_tx);
        let (frame_tx, mut frame_rx) = mpsc::channel(4);

        stream_runtime_frames(initial, event_rx, frame_tx, CancellationToken::new()).await;

        let first = frame_rx.recv().await.expect("snapshot frame");
        let RuntimeWatchFrame::Snapshot { state } = first else {
            panic!("first frame should be a snapshot");
        };
        assert_eq!(
            state
                .instances
                .iter()
                .map(|record| record.instance_id.0.as_str())
                .collect::<Vec<_>>(),
            ["instance-a", "instance-b"]
        );

        let second = frame_rx.recv().await.expect("event frame");
        assert_eq!(
            second,
            RuntimeWatchFrame::Upsert {
                collection: RuntimeCollection::Instance,
                key: String::from("instance-c"),
                record: RuntimeRecord::Instance(instance_record("instance-c", "prod", "worker")),
            }
        );
    }

    #[tokio::test]
    async fn stream_runtime_frames_emits_error_frame_before_closing() {
        let initial = RoutingState {
            machines: Vec::new(),
            revisions: Vec::new(),
            releases: Vec::new(),
            instances: Vec::new(),
        };
        let (event_tx, event_rx) = mpsc::channel(1);
        event_tx
            .send(Err(String::from("routing consumer failed")))
            .await
            .expect("queue failure");
        drop(event_tx);
        let (frame_tx, mut frame_rx) = mpsc::channel(4);

        stream_runtime_frames(initial, event_rx, frame_tx, CancellationToken::new()).await;

        let first = frame_rx.recv().await.expect("snapshot frame");
        assert!(matches!(first, RuntimeWatchFrame::Snapshot { .. }));
        let second = frame_rx.recv().await.expect("error frame");
        assert_eq!(
            second,
            RuntimeWatchFrame::Error {
                code: String::from("RUNTIME_SUBSCRIPTION_FAILED"),
                message: String::from("routing consumer failed"),
            }
        );
        assert!(frame_rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn relay_runtime_events_ack_and_exit_when_receiver_closes() {
        let (envelope_tx, mut envelope_rx) = mpsc::channel(1);
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        envelope_tx
            .send(Ok(RoutingEventEnvelope::with_ack(
                "event-1",
                None,
                RoutingEvent::InstanceAdded(instance_record("instance-a", "prod", "web")),
                ack_tx,
            )))
            .await
            .expect("queue envelope");
        drop(envelope_tx);
        let (event_tx, event_rx) = mpsc::channel(1);
        drop(event_rx);

        relay_runtime_events(&mut envelope_rx, event_tx).await;

        ack_rx.await.expect("event should be acked");
    }

    #[tokio::test]
    async fn relay_runtime_events_exit_on_subscription_failure() {
        let (envelope_tx, mut envelope_rx) = mpsc::channel(1);
        envelope_tx
            .send(Err(ployz_types::Error::operation(
                "test_routing_subscription",
                "closed",
            )))
            .await
            .expect("queue failure");
        drop(envelope_tx);
        let (event_tx, mut event_rx) = mpsc::channel(1);

        relay_runtime_events(&mut envelope_rx, event_tx).await;

        assert_eq!(
            event_rx.recv().await,
            Some(Err(String::from("test_routing_subscription: closed")))
        );
        assert!(event_rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn relay_runtime_events_surface_ack_failure_to_runtime_reader() {
        let (envelope_tx, mut envelope_rx) = mpsc::channel(1);
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        drop(ack_rx);
        envelope_tx
            .send(Ok(RoutingEventEnvelope::with_ack(
                "event-ack-failed",
                None,
                RoutingEvent::InstanceAdded(instance_record("instance-a", "prod", "web")),
                ack_tx,
            )))
            .await
            .expect("queue envelope");
        drop(envelope_tx);
        let (event_tx, mut event_rx) = mpsc::channel(2);

        relay_runtime_events(&mut envelope_rx, event_tx).await;

        assert!(
            event_rx
                .recv()
                .await
                .expect("event should be forwarded before ack failure")
                .is_ok()
        );
        let error = event_rx
            .recv()
            .await
            .expect("ack failure should be forwarded")
            .expect_err("ack failure should be visible to runtime reader");
        assert!(error.contains("event-ack-failed"));
        assert!(error.contains("ack receiver closed"));
        assert!(event_rx.recv().await.is_none());
    }

    fn instance_record(id: &str, namespace: &str, service: &str) -> InstanceStatusRecord {
        let mut backend_ports = BTreeMap::new();
        backend_ports.insert(String::from("http"), 8080);
        InstanceStatusRecord {
            instance_id: InstanceId(id.into()),
            namespace: Namespace(namespace.into()),
            service: service.into(),
            slot_id: SlotId(String::from("slot-1")),
            machine_id: MachineId(String::from("machine-1")),
            revision_hash: String::from("rev-1"),
            deploy_id: DeployId(String::from("deploy-1")),
            docker_container_id: String::from("container-1"),
            overlay_ip: Some(Ipv4Addr::new(10, 0, 0, 2)),
            backend_ports,
            phase: InstancePhase::Ready,
            ready: false,
            drain_state: DrainState::None,
            error: None,
            started_at: 10,
            updated_at: 20,
        }
    }
}
