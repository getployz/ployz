use std::time::Duration;

use ployz_api::{RuntimeWatchFrame, runtime_frame_from_event, sort_routing_state};
use ployz_store_api::RoutingSnapshotReader;
use ployz_types::model::{RoutingEvent, RoutingState};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::daemon::DaemonState;

impl DaemonState {
    pub async fn open_runtime_subscription(
        &self,
    ) -> Result<(RoutingState, mpsc::Receiver<RoutingEvent>), Box<ployz_api::DaemonResponse>> {
        let active = self.require_active("NO_MESH", "no mesh is running")?;
        let (state, mut batches) = active
            .mesh
            .store
            .subscribe_routing_batches("ployzd.runtime")
            .await
            .map_err(|error| Box::new(self.err("RUNTIME_SUBSCRIBE_FAILED", error.to_string())))?;
        let (tx, rx) = mpsc::channel(1024);
        tokio::spawn(async move {
            while let Some(batch) = batches.recv().await {
                let mut sent_all = true;
                for event in batch.events.clone() {
                    if tx.send(event).await.is_err() {
                        sent_all = false;
                        break;
                    }
                }
                if sent_all {
                    let _ = batch.ack().await;
                }
            }
        });
        Ok((state, rx))
    }
}

pub async fn stream_runtime_frames(
    mut initial: RoutingState,
    mut events: mpsc::Receiver<RoutingEvent>,
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
                let frame = runtime_frame_from_event(event);
                if frames.send(frame).await.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::stream_runtime_frames;
    use ployz_api::{RuntimeRecord, RuntimeTable, RuntimeWatchFrame};
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
            .send(RoutingEvent::InstanceAdded(instance_record(
                "instance-c",
                "prod",
                "worker",
            )))
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
                table: RuntimeTable::Instance,
                key: String::from("instance-c"),
                record: RuntimeRecord::Instance(instance_record("instance-c", "prod", "worker")),
            }
        );
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
