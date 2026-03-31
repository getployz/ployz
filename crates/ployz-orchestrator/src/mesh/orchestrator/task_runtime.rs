use super::*;
use crate::mesh::probe::run_probe_listener_task;
use crate::mesh::tasks::TaskSetError;
use crate::mesh::tasks::{
    run_ebpf_sync_task, run_endpoint_refresh_task, run_heartbeat_task, run_participation_task,
    run_peer_sync_task, run_self_liveness_task, run_self_record_writer_task,
    run_subnet_claim_monitor_task,
};
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;

impl Mesh {
    pub(crate) async fn start_peer_sync_task(&mut self) -> Result<()> {
        if self.peer_sync_tx.is_some() {
            return Ok(());
        }

        let (snapshot, events) = self
            .store
            .subscribe_machines()
            .await
            .map_err(TaskSetError::Subscribe)?;
        let (peer_sync_tx, peer_sync_rx) = mpsc::channel(64);
        let (mut task_set, cancel) = TaskSet::new();
        let bootstrap_peers: Vec<_> = self
            .seed_records
            .iter()
            .filter(|machine| machine.id != self.machine_id)
            .cloned()
            .collect();
        if self.network.runs_probe_listener() {
            self.probe_readiness.clear_bound_families();
            task_set.spawn(run_probe_listener_task(
                cancel.clone(),
                Arc::clone(&self.probe_readiness),
            ));
        }
        task_set.spawn(run_peer_sync_task(
            snapshot,
            events,
            peer_sync_rx,
            bootstrap_peers,
            self.network.clone(),
            self.machine_id.clone(),
            cancel.clone(),
        ));

        self.peer_sync_tx = Some(peer_sync_tx);
        self.task_cancel = Some(cancel);
        self.tasks = Some(task_set);
        Ok(())
    }

    pub(crate) async fn spawn_background_tasks(&mut self) -> Result<()> {
        let Some(cancel) = self.task_cancel.clone() else {
            return Err(TaskSetError::Subscribe(crate::error::Error::operation(
                "spawn_background_tasks",
                "peer sync task not started (no cancel token)".to_string(),
            ))
            .into());
        };

        if self.authoritative_self.is_none() {
            let store_self = self.store.list_machines().await.ok().and_then(|machines| {
                machines
                    .into_iter()
                    .find(|machine| machine.id == self.machine_id)
            });
            let authoritative = self
                .seed_records
                .iter()
                .find(|machine| machine.id == self.machine_id)
                .cloned()
                .or(store_self)
                .ok_or_else(|| {
                    TaskSetError::Subscribe(crate::error::Error::operation(
                        "self machine record",
                        "authoritative self record missing".to_string(),
                    ))
                })?;
            self.authoritative_self = Some(Arc::new(RwLock::new(authoritative)));
        }
        let authoritative_self =
            self.authoritative_self
                .clone()
                .ok_or(TaskSetError::InternalState(
                    "authoritative self record missing after initialization",
                ))?;
        self.sync_required_probe_family(authoritative_self.read().await.overlay_ip);

        let task_set = self.tasks.as_mut().ok_or(TaskSetError::InternalState(
            "task set missing before background task spawn",
        ))?;

        let (self_record_tx, self_record_rx) = mpsc::channel(64);
        self.self_record_tx = Some(self_record_tx.clone());
        task_set.spawn(run_self_record_writer_task(
            authoritative_self.clone(),
            Arc::clone(&self.store),
            self_record_rx,
            cancel.clone(),
        ));

        task_set.spawn(run_endpoint_refresh_task(
            self.machine_id.clone(),
            self.listen_port,
            self.endpoint_discovery.clone(),
            authoritative_self.clone(),
            self_record_tx.clone(),
            cancel.clone(),
        ));

        self.heartbeat_started.store(false, Ordering::SeqCst);
        let (self_liveness_tx, self_liveness_rx) = mpsc::channel(16);
        self.self_liveness_tx = Some(self_liveness_tx.clone());
        task_set.spawn(run_self_liveness_task(
            self.network.clone(),
            self.heartbeat_started.clone(),
            self_record_tx.clone(),
            self_liveness_rx,
            cancel.clone(),
        ));

        let (participation_tx, participation_rx) = mpsc::channel(16);
        self.participation_tx = Some(participation_tx.clone());
        task_set.spawn(run_participation_task(
            self.machine_id.clone(),
            authoritative_self.clone(),
            Arc::clone(&self.store),
            self.network.clone(),
            Arc::clone(&self.probe_readiness),
            self_record_tx,
            participation_rx,
            cancel.clone(),
        ));

        let (heartbeat_tx, heartbeat_rx) = mpsc::channel(16);
        self.heartbeat_tx = Some(heartbeat_tx);
        task_set.spawn(run_heartbeat_task(
            self_liveness_tx,
            participation_tx,
            heartbeat_rx,
            cancel.clone(),
        ));

        let (subnet_snapshot, subnet_events) = self
            .store
            .subscribe_machines()
            .await
            .map_err(TaskSetError::Subscribe)?;
        task_set.spawn(run_subnet_claim_monitor_task(
            subnet_snapshot,
            subnet_events,
            cancel.clone(),
        ));

        if let Some(ref dataplane) = self.dataplane {
            let (ebpf_snapshot, ebpf_events) = self
                .store
                .subscribe_machines()
                .await
                .map_err(TaskSetError::Subscribe)?;
            task_set.spawn(run_ebpf_sync_task(
                ebpf_snapshot,
                ebpf_events,
                dataplane.clone(),
                self.wg_ifindex,
                self.machine_id.clone(),
                cancel.clone(),
            ));
        }

        Ok(())
    }

    pub(crate) fn clear_task_channels(&mut self) {
        self.peer_sync_tx = None;
        self.heartbeat_tx = None;
        self.self_liveness_tx = None;
        self.participation_tx = None;
        self.self_record_tx = None;
        self.task_cancel = None;
        self.heartbeat_started.store(false, Ordering::SeqCst);
    }
}
