use ployz_api::{DaemonResponse, DebugTickTask};

use crate::daemon::DaemonState;

impl DaemonState {
    pub(crate) async fn handle_debug_tick(
        &mut self,
        task: DebugTickTask,
        repeat: u32,
    ) -> DaemonResponse {
        if repeat == 0 {
            return self.err("INVALID_ARGUMENT", "debug tick requires repeat >= 1");
        }

        for _ in 0..repeat {
            let result = match task {
                DebugTickTask::PeerSync => self.debug_tick_peer_sync(),
                DebugTickTask::Heartbeat => self.debug_tick_heartbeat().await,
                DebugTickTask::Heal => self.debug_tick_heal().await,
                DebugTickTask::All => {
                    if let Err(error) = self.debug_tick_heartbeat().await {
                        Err(error)
                    } else {
                        self.debug_tick_heal().await
                    }
                }
            };
            if let Err((code, message)) = result {
                return self.err(code, message);
            }
        }

        self.ok(format!(
            "debug tick complete task={} repeat={repeat}",
            format_debug_tick_task(task)
        ))
    }

    fn debug_tick_peer_sync(&self) -> Result<(), (&'static str, String)> {
        Err((
            "UNSUPPORTED_OPERATION",
            "peer sync is event-driven and no longer supports manual ticks".into(),
        ))
    }

    async fn debug_tick_heartbeat(&mut self) -> Result<(), (&'static str, String)> {
        let Some(active) = self.active.as_ref() else {
            return Err(("NO_RUNNING_NETWORK", "no mesh running".into()));
        };
        if active.mesh.publish_up_once().await.is_none() {
            return Err(("TASK_NOT_RUNNING", "heartbeat task is not running".into()));
        }
        Ok(())
    }

    async fn debug_tick_heal(&mut self) -> Result<(), (&'static str, String)> {
        self.heal_local_subnet_conflict_if_needed().await;
        Ok(())
    }
}

fn format_debug_tick_task(task: DebugTickTask) -> &'static str {
    match task {
        DebugTickTask::PeerSync => "peer-sync",
        DebugTickTask::Heartbeat => "heartbeat",
        DebugTickTask::Heal => "heal",
        DebugTickTask::All => "all",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh_state::network::DEFAULT_CLUSTER_CIDR;
    use ployz_api::DebugTickTask;
    use ployz_runtime_api::Identity;

    #[tokio::test]
    async fn debug_tick_rejects_when_no_mesh_is_running() {
        let mut state = DaemonState::new_for_tests(
            &std::env::temp_dir().join("ployz-debug-tick-no-mesh"),
            Identity::generate(ployz_types::model::MachineId("self".into()), [1; 32]),
            DEFAULT_CLUSTER_CIDR.into(),
            24,
            4317,
            "127.0.0.1:0".into(),
            1,
        );

        let response = state.handle_debug_tick(DebugTickTask::All, 1).await;
        assert!(!response.ok);
        assert_eq!(response.code, "NO_RUNNING_NETWORK");
    }
}
