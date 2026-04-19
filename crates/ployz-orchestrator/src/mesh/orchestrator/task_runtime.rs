use super::*;
use crate::mesh::probe::run_probe_listener_task;
use crate::mesh::tasks::TaskSetError;
use crate::mesh::tasks::{
    SelfRecordMutation, apply_self_record_mutation, run_ebpf_sync_task, run_endpoint_refresh_task,
    run_peer_sync_task, run_self_record_writer_task, run_subnet_claim_monitor_task,
};
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
            self.task_timing.peer_sync_interval,
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
        let initial_self_record = authoritative_self.read().await.clone();
        let published = apply_self_record_mutation(
            &self_record_tx,
            SelfRecordMutation::Replace(initial_self_record),
        )
        .await;
        if published.is_none() {
            return Err(TaskSetError::Subscribe(crate::error::Error::operation(
                "self machine record",
                "initial self record publish failed".to_string(),
            ))
            .into());
        }

        task_set.spawn(run_endpoint_refresh_task(
            self.machine_id.clone(),
            self.listen_port,
            self.endpoint_discovery.clone(),
            authoritative_self.clone(),
            self_record_tx.clone(),
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
        self.self_record_tx = None;
        self.task_cancel = None;
    }
}
