//! Tiny all-node loop that maintains the advisory preferred controller.

use std::sync::Arc;
use std::time::Duration;

use ployz_core::corrosion::{CorrosionTimestamp, controller_heartbeat_is_stale};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
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
                        if matches!(error, KernelError::Controller(ControllerStoreError::InsufficientVisibility)) {
                            tracing::debug!("controller heartbeat paused by visibility brake");
                        } else {
                            tracing::warn!(%error, "controller kernel tick failed");
                        }
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
            .any(|machine| &machine.id == local_machine_id)
        {
            return Ok(());
        }
        let current = self.controller.current().await?;
        let now = now()?;
        match current {
            None => {
                self.controller
                    .initial_self_appointment(roster.machines.len(), now)
                    .await?;
            }
            Some(current) if &current.preferred_machine_id == local_machine_id => {
                self.controller
                    .heartbeat(roster.machines.len(), &current, now)
                    .await?;
            }
            Some(current)
                if controller_heartbeat_is_stale(now, current.heartbeat_at, STALE_AFTER) =>
            {
                self.controller
                    .appoint_self_if_current_is_stale(roster.machines.len(), &current, now)
                    .await?;
            }
            Some(_) => {}
        }
        Ok(())
    }
}

fn now() -> Result<CorrosionTimestamp, KernelError> {
    let value = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| KernelError::Timestamp(error.to_string()))?;
    CorrosionTimestamp::try_new(value).map_err(|error| KernelError::Timestamp(error.to_string()))
}

#[derive(Debug, thiserror::Error)]
enum KernelError {
    #[error("could not read the accepted roster: {0}")]
    Roster(#[from] MutationStoreError),
    #[error("could not update the controller appointment: {0}")]
    Controller(#[from] ControllerStoreError),
    #[error("could not read the wall clock: {0}")]
    Timestamp(String),
}
