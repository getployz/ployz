use std::sync::Arc;

use ployz_store_api::MachineStore;
use tokio::sync::{RwLock, mpsc};

use crate::mesh::MeshNetwork;
use crate::mesh::probe::run_probe_listener_task;
use crate::mesh::tasks::{
    SelfRecordMutation, TaskSetError, apply_self_record_mutation, run_ebpf_sync_task,
    run_peer_sync_task, run_self_record_writer_task, run_subnet_claim_monitor_task,
};

use super::{Mesh, Result};

impl Mesh {
    pub(super) async fn start_peer_sync_task(&mut self) -> Result<()> {
        if self.peer_sync_tx.is_some() {
            return Ok(());
        }

        let (snapshot, events) = self
            .store
            .subscribe_machines()
            .await
            .map_err(TaskSetError::Subscribe)?;
        let (peer_sync_tx, peer_sync_rx) = mpsc::channel(64);
        let (mut task_set, cancel) = crate::mesh::tasks::TaskSet::new();
        let bootstrap_peers: Vec<_> = self
            .seed_records
            .iter()
            .filter(|machine| machine.id != self.machine_id)
            .cloned()
            .collect();
        if self.network.runs_probe_listener() {
            task_set.spawn(run_probe_listener_task(cancel.clone()));
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

    pub(super) async fn spawn_background_tasks(&mut self) -> Result<()> {
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
        let authoritative_self = self.authoritative_self.clone().expect("set above");
        let task_set = self
            .tasks
            .as_mut()
            .expect("tasks set by start_peer_sync_task");

        let (self_record_tx, self_record_rx) = mpsc::channel(64);
        self.self_record_tx = Some(self_record_tx.clone());
        task_set.spawn(run_self_record_writer_task(
            authoritative_self.clone(),
            self.store.clone(),
            self_record_rx,
            cancel.clone(),
        ));
        let bridge_ip = self.network.bridge_ip().await;
        let _ = apply_self_record_mutation(
            &self_record_tx,
            SelfRecordMutation::PublishUp { bridge_ip },
        )
        .await;

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
}
