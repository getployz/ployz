use ployz_api::{DaemonResponse, DebugTickTask};
use ployz_orchestrator::mesh::wireguard::DEFAULT_LISTEN_PORT;
use ployz_orchestrator::network::endpoints::detect_advertised_endpoints;

use crate::daemon::DaemonState;
use crate::endpoint_maintenance::publish_local_endpoints_for_mesh;

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
                DebugTickTask::Endpoints => self.debug_tick_endpoints().await,
                DebugTickTask::Heartbeat => self.debug_tick_heartbeat().await,
                DebugTickTask::All => {
                    if let Err(error) = self.debug_tick_heartbeat().await {
                        Err(error)
                    } else if let Err(error) = self.debug_tick_endpoints().await {
                        Err(error)
                    } else {
                        Ok(())
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

    async fn debug_tick_endpoints(&mut self) -> Result<(), (&'static str, String)> {
        let Some(active) = self.active.as_ref() else {
            return Err(("NO_RUNNING_NETWORK", "no mesh running".into()));
        };
        let detected_endpoints = detect_advertised_endpoints(DEFAULT_LISTEN_PORT).await;

        publish_local_endpoints_for_mesh(&active.mesh, detected_endpoints)
            .await
            .map_err(|error| ("ENDPOINT_UPDATE_FAILED", error))?;

        // Manual debug ticks are allowed to force a one-shot endpoint rotation
        // pass so operators and tests do not have to wait through the normal
        // down interval before trying the next declared candidate.
        if !active.mesh.run_endpoint_maintenance_once(true).await {
            return Err((
                "TASK_NOT_RUNNING",
                "endpoint maintainer task is not running".into(),
            ));
        }

        Ok(())
    }
}

fn format_debug_tick_task(task: DebugTickTask) -> &'static str {
    match task {
        DebugTickTask::PeerSync => "peer-sync",
        DebugTickTask::Endpoints => "endpoints",
        DebugTickTask::Heartbeat => "heartbeat",
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
            Identity::generate(ployz_types::model::MachineId::new("self"), [1; 32]),
            DEFAULT_CLUSTER_CIDR.into(),
            24,
            4319,
            "127.0.0.1:0".into(),
            None,
            1,
        );

        let response = state.handle_debug_tick(DebugTickTask::All, 1).await;
        assert!(!response.ok);
        assert_eq!(response.code, "NO_RUNNING_NETWORK");
    }

    #[tokio::test]
    async fn endpoint_debug_tick_rejects_when_no_mesh_is_running() {
        let mut state = DaemonState::new_for_tests(
            &std::env::temp_dir().join("ployz-debug-tick-no-mesh-endpoints"),
            Identity::generate(ployz_types::model::MachineId::new("self"), [1; 32]),
            DEFAULT_CLUSTER_CIDR.into(),
            24,
            4319,
            "127.0.0.1:0".into(),
            None,
            1,
        );

        let response = state.handle_debug_tick(DebugTickTask::Endpoints, 1).await;
        assert!(!response.ok);
        assert_eq!(response.code, "NO_RUNNING_NETWORK");
    }
}
