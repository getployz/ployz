use std::sync::Arc;
use std::time::Duration;

use ployz_orchestrator::Mesh;
use ployz_orchestrator::mesh::wireguard::DEFAULT_LISTEN_PORT;
use ployz_orchestrator::network::endpoints::detect_advertised_endpoints;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
#[cfg(target_os = "linux")]
use tracing::debug;

use crate::daemon::DaemonState;

#[cfg(target_os = "linux")]
use std::process::Stdio;
#[cfg(target_os = "linux")]
use tokio::io::{AsyncBufReadExt, BufReader};
#[cfg(target_os = "linux")]
use tokio::process::Command;

const LOCAL_ENDPOINT_AUDIT_INTERVAL: Duration = Duration::from_secs(60);

pub(crate) fn spawn_local_endpoint_maintenance(
    state: Arc<RwLock<DaemonState>>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let mut audit = tokio::time::interval(LOCAL_ENDPOINT_AUDIT_INTERVAL);
        let mut watcher = spawn_linux_endpoint_watcher();

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = audit.tick() => {
                    reconcile_local_endpoints_from_state(&state).await;
                }
                event = watcher.next() => {
                    match event {
                        Some(EndpointWatchEvent::Changed) => {
                            reconcile_local_endpoints_from_state(&state).await;
                        }
                        Some(EndpointWatchEvent::Closed) => {
                            watcher = EndpointWatcher::unsupported();
                        }
                        None => {}
                    }
                }
            }
        }
    });
}

pub(crate) fn local_endpoint_watch_supported() -> bool {
    cfg!(target_os = "linux")
}

pub(crate) async fn reconcile_local_endpoints_for_mesh(
    mesh: &Mesh,
    detected_endpoints: Vec<String>,
) -> Result<Option<Vec<String>>, String> {
    let Some(current_record) = mesh.authoritative_self_record().await else {
        return Err("mesh self record unavailable".into());
    };

    if current_record.endpoints == detected_endpoints {
        return Ok(None);
    }

    match mesh
        .update_authoritative_self_record(|record| {
            record.endpoints = detected_endpoints.clone();
        })
        .await
    {
        Some(_) => Ok(Some(detected_endpoints)),
        None => Err("failed to update advertised self endpoints".into()),
    }
}

async fn reconcile_local_endpoints_from_state(state: &Arc<RwLock<DaemonState>>) {
    let detected_endpoints = detect_advertised_endpoints(DEFAULT_LISTEN_PORT).await;
    let result = {
        let state_guard = state.read().await;
        let Some(active) = state_guard.active.as_ref() else {
            return;
        };
        reconcile_local_endpoints_for_mesh(&active.mesh, detected_endpoints).await
    };

    match result {
        Ok(Some(detected_endpoints)) => {
            info!(endpoints = ?detected_endpoints, "updated advertised self endpoints");
        }
        Ok(None) => {}
        Err(error) => {
            warn!(%error, "failed to reconcile advertised self endpoints");
        }
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum EndpointWatchEvent {
    Changed,
    Closed,
}

struct EndpointWatcher {
    #[cfg(target_os = "linux")]
    child: Option<tokio::process::Child>,
    #[cfg(target_os = "linux")]
    lines: Option<tokio::io::Lines<BufReader<tokio::process::ChildStdout>>>,
}

impl EndpointWatcher {
    fn unsupported() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            child: None,
            #[cfg(target_os = "linux")]
            lines: None,
        }
    }

    async fn next(&mut self) -> Option<EndpointWatchEvent> {
        #[cfg(not(target_os = "linux"))]
        {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            return None;
        }

        #[cfg(target_os = "linux")]
        {
        let Some(lines) = &mut self.lines else {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            return None;
        };

        match lines.next_line().await {
            Ok(Some(line)) => {
                debug!(line = %line, "local endpoint watcher event");
                Some(EndpointWatchEvent::Changed)
            }
            Ok(None) => {
                if let Some(mut child) = self.child.take() {
                    let _ = child.wait().await;
                }
                self.lines = None;
                Some(EndpointWatchEvent::Closed)
            }
            Err(error) => {
                warn!(?error, "local endpoint watcher read failed");
                if let Some(mut child) = self.child.take() {
                    let _ = child.wait().await;
                }
                self.lines = None;
                Some(EndpointWatchEvent::Closed)
            }
        }
        }
    }
}

fn spawn_linux_endpoint_watcher() -> EndpointWatcher {
    #[cfg(target_os = "linux")]
    {
        let mut child = match Command::new("ip")
            .args(["monitor", "address", "link"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                warn!(?error, "failed to start local endpoint watcher");
                return EndpointWatcher::unsupported();
            }
        };
        let Some(stdout) = child.stdout.take() else {
            return EndpointWatcher::unsupported();
        };
        return EndpointWatcher {
            child: Some(child),
            lines: Some(BufReader::new(stdout).lines()),
        };
    }

    #[cfg(not(target_os = "linux"))]
    {
        EndpointWatcher::unsupported()
    }
}
