//! Tiny all-node loop that maintains the advisory preferred controller.

use std::sync::Arc;
use std::time::Duration;

use ployz_core::corrosion::{CorrosionTimestamp, controller_heartbeat_is_stale};
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;

use super::controller::{ControllerStore, ControllerStoreError};
use super::store::{MutationStoreError, read_accepted_roster};
use crate::corrosion::CorrosionClient;

const TICK_INTERVAL: Duration = Duration::from_secs(5);
const STALE_AFTER: Duration = Duration::from_secs(30);

pub(super) struct ControllerKernel {
    corrosion: CorrosionClient,
    controller: Arc<ControllerStore>,
}

impl ControllerKernel {
    pub(super) fn new(corrosion: CorrosionClient, controller: Arc<ControllerStore>) -> Self {
        Self {
            corrosion,
            controller,
        }
    }

    pub(super) async fn run(self, mut shutdown: watch::Receiver<bool>) {
        let mut interval = tokio::time::interval(TICK_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(error) = self.tick().await {
                        tracing::warn!(%error, "controller kernel tick failed");
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                }
            }
        }
    }

    async fn tick(&self) -> Result<(), KernelError> {
        let roster = read_accepted_roster(&self.corrosion, self.controller.cluster_id()).await?;
        let local_machine_id = self.controller.local_machine_id();
        if !roster
            .machines
            .iter()
            .any(|machine| &machine.document.name == local_machine_id)
        {
            return Ok(());
        }
        let current = self.controller.current().await?;
        let now = CorrosionTimestamp::now_utc();
        match current {
            None => {
                self.controller.initial_self_appointment(now).await?;
            }
            Some(current) if &current.preferred_machine_id == local_machine_id => {
                self.controller.heartbeat(&current, now).await?;
            }
            Some(current)
                if controller_heartbeat_is_stale(now, current.heartbeat_at, STALE_AFTER) =>
            {
                self.controller
                    .appoint_self_if_current_is_stale(&current, now)
                    .await?;
            }
            Some(_) => {}
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
enum KernelError {
    #[error("could not read the accepted roster: {0}")]
    Roster(#[from] MutationStoreError),
    #[error("could not update the controller appointment: {0}")]
    Controller(#[from] ControllerStoreError),
}
