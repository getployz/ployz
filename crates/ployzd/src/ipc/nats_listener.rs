use futures_util::StreamExt;
use ployz_nats::{decode_node_request, encode_node_response};
use ployz_runtime_api::RuntimeHandle;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::listener::{IncomingCommand, IncomingRequest};

const RESUBSCRIBE_DELAY: Duration = Duration::from_secs(1);
const MAX_IN_FLIGHT_COMMANDS: usize = 64;
pub const NATS_NODE_RPC_HEALTH_FILE: &str = "nats-node-rpc-health.json";

pub type NatsNodeRpcHealth = crate::health::ComponentHealth;

pub struct NatsListenerHandle {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl NatsListenerHandle {
    #[must_use]
    pub fn noop() -> Self {
        Self {
            cancel: CancellationToken::new(),
            task: tokio::spawn(async {}),
        }
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

#[async_trait::async_trait]
impl RuntimeHandle for NatsListenerHandle {
    async fn shutdown(self: Box<Self>) -> std::result::Result<(), String> {
        NatsListenerHandle::shutdown(*self).await;
        Ok(())
    }
}

pub async fn serve(
    client: async_nats::Client,
    subject: String,
    queue_group: String,
    tx: mpsc::Sender<IncomingCommand>,
    health_path: PathBuf,
) -> Result<NatsListenerHandle, String> {
    let mut subscriber = client
        .queue_subscribe(subject.clone(), queue_group.clone())
        .await
        .map_err(|error| format!("queue subscribe {subject} {queue_group}: {error}"))?;
    let health = Arc::new(NodeRpcHealthRecorder::new(health_path));
    health.force_healthy().await;
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        let permits = Arc::new(Semaphore::new(MAX_IN_FLIGHT_COMMANDS));
        info!(%subject, %queue_group, max_in_flight = MAX_IN_FLIGHT_COMMANDS, "nats node rpc listener subscribed");
        loop {
            tokio::select! {
                _ = task_cancel.cancelled() => {
                    info!(%subject, %queue_group, "nats node rpc listener shutting down");
                    break;
                }
                next = subscriber.next() => {
                    let Some(message) = next else {
                        warn!(%subject, %queue_group, "nats node rpc subscription closed; resubscribing");
                        health.record_unhealthy("nats node rpc subscription closed").await;
                        subscriber = match resubscribe(
                            &client,
                            &subject,
                            &queue_group,
                            &task_cancel,
                            health.clone(),
                        ).await {
                            Some(subscriber) => {
                                health.force_healthy().await;
                                subscriber
                            }
                            None => break,
                        };
                        continue;
                    };
                    let permit = tokio::select! {
                        _ = task_cancel.cancelled() => break,
                        permit = permits.clone().acquire_owned() => match permit {
                            Ok(permit) => permit,
                            Err(error) => {
                                warn!(%error, "nats node rpc concurrency limiter closed");
                                break;
                            }
                        },
                    };
                    let client = client.clone();
                    let tx = tx.clone();
                    let health = health.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        if let Err(error) = handle_message(client, message, tx).await {
                            warn!(%error, "nats node rpc message failed");
                            health
                                .record_unhealthy(format!("nats node rpc message failed: {error}"))
                                .await;
                        } else {
                            health.record_healthy_if_stale().await;
                        }
                    });
                }
            }
        }
    });
    Ok(NatsListenerHandle { cancel, task })
}

async fn resubscribe(
    client: &async_nats::Client,
    subject: &str,
    queue_group: &str,
    cancel: &CancellationToken,
    health: Arc<NodeRpcHealthRecorder>,
) -> Option<async_nats::Subscriber> {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return None,
            _ = tokio::time::sleep(RESUBSCRIBE_DELAY) => {}
        }
        match client
            .queue_subscribe(subject.to_string(), queue_group.to_string())
            .await
        {
            Ok(subscriber) => {
                info!(%subject, %queue_group, "nats node rpc listener resubscribed");
                return Some(subscriber);
            }
            Err(error) => {
                warn!(%subject, %queue_group, %error, "nats node rpc resubscribe failed");
                health
                    .record_unhealthy(format!("nats node rpc resubscribe failed: {error}"))
                    .await;
            }
        }
    }
}

pub async fn load_health(path: PathBuf) -> std::io::Result<NatsNodeRpcHealth> {
    let bytes = tokio::fs::read(path).await?;
    serde_json::from_slice(&bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

struct NodeRpcHealthRecorder {
    path: PathBuf,
    state: Mutex<NodeRpcHealthState>,
}

#[derive(Debug, Default)]
struct NodeRpcHealthState {
    health: Option<NatsNodeRpcHealth>,
}

impl NodeRpcHealthRecorder {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            state: Mutex::new(NodeRpcHealthState::default()),
        }
    }

    async fn force_healthy(&self) {
        let mut state = self.state.lock().await;
        state.health = None;
        write_health(&self.path, healthy_health()).await;
    }

    async fn record_healthy_if_stale(&self) {
        let mut state = self.state.lock().await;
        if state.health.is_none() {
            return;
        }
        state.health = None;
        write_health(&self.path, healthy_health()).await;
    }

    async fn record_unhealthy(&self, error: impl Into<String>) {
        let mut state = self.state.lock().await;
        let now = unix_secs();
        let next = NatsNodeRpcHealth::stale(now, state.health.as_ref(), error);
        state.health = Some(next.clone());
        write_health(&self.path, next).await;
    }
}

#[cfg(test)]
async fn record_unhealthy(
    path: &PathBuf,
    health_state: &mut Option<NatsNodeRpcHealth>,
    error: impl Into<String>,
) {
    let now = unix_secs();
    let next = NatsNodeRpcHealth::stale(now, health_state.as_ref(), error);
    *health_state = Some(next.clone());
    write_health(path, next).await;
}

fn healthy_health() -> NatsNodeRpcHealth {
    NatsNodeRpcHealth::healthy(unix_secs())
}

async fn write_health(path: &PathBuf, health: NatsNodeRpcHealth) {
    let Ok(payload) = serde_json::to_vec_pretty(&health) else {
        return;
    };
    if let Some(parent) = path.parent()
        && let Err(error) = tokio::fs::create_dir_all(parent).await
    {
        warn!(path = %path.display(), %error, "failed to create nats node rpc health directory");
        return;
    }
    if let Err(error) = tokio::fs::write(path, payload).await {
        warn!(path = %path.display(), %error, "failed to write nats node rpc health");
    } else {
        debug!(path = %path.display(), "wrote nats node rpc health");
    }
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::{
        NatsNodeRpcHealth, NodeRpcHealthRecorder, healthy_health, load_health, record_unhealthy,
        write_health,
    };
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    #[tokio::test]
    async fn node_rpc_health_roundtrip() {
        let path = temp_path("node-rpc-health-roundtrip").join("health.json");
        let first = NatsNodeRpcHealth::stale(1_777_646_000, None, "first");
        let health = NatsNodeRpcHealth::stale(1_777_646_100, Some(&first), "subscription closed");

        write_health(&path, health.clone()).await;

        let loaded = load_health(path).await.expect("load health");
        assert_eq!(loaded, health);
    }

    #[tokio::test]
    async fn node_rpc_unhealthy_keeps_original_stale_since() {
        let path = temp_path("node-rpc-health-stale").join("health.json");
        let mut health_state = None;

        record_unhealthy(&path, &mut health_state, "first").await;
        let first = load_health(path.clone()).await.expect("load first health");
        record_unhealthy(&path, &mut health_state, "second").await;
        let second = load_health(path).await.expect("load second health");

        let crate::health::ComponentHealthState::Stale {
            stale_since_unix_secs: first_stale_since,
            ..
        } = first.state
        else {
            panic!("first health should be stale");
        };
        let crate::health::ComponentHealthState::Stale {
            stale_since_unix_secs: second_stale_since,
            consecutive_failures,
            last_error,
        } = second.state
        else {
            panic!("second health should be stale");
        };
        assert_eq!(first_stale_since, second_stale_since);
        assert_eq!(consecutive_failures, 2);
        assert_eq!(last_error, "second");
    }

    #[test]
    fn node_rpc_healthy_state_is_fresh() {
        let health = healthy_health();

        assert_eq!(health.state, crate::health::ComponentHealthState::Healthy);
    }

    #[tokio::test]
    async fn node_rpc_recorder_clears_stale_health_after_success() {
        let path = temp_path("node-rpc-health-recorder").join("health.json");
        let recorder = NodeRpcHealthRecorder::new(path.clone());

        recorder.force_healthy().await;
        recorder.record_unhealthy("publish response failed").await;
        let unhealthy = load_health(path.clone()).await.expect("load unhealthy");

        let crate::health::ComponentHealthState::Stale {
            consecutive_failures,
            last_error,
            ..
        } = unhealthy.state
        else {
            panic!("unhealthy record should be stale");
        };
        assert_eq!(consecutive_failures, 1);
        assert_eq!(last_error, "publish response failed");

        recorder.record_healthy_if_stale().await;
        let healthy = load_health(path).await.expect("load healthy");

        assert_eq!(healthy.state, crate::health::ComponentHealthState::Healthy);
    }

    fn temp_path(label: &str) -> PathBuf {
        let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}-{sequence}", std::process::id()))
    }
}

async fn handle_message(
    client: async_nats::Client,
    message: async_nats::Message,
    tx: mpsc::Sender<IncomingCommand>,
) -> Result<(), String> {
    let Some(reply) = message.reply.clone() else {
        warn!("nats node rpc request missing reply subject");
        return Ok(());
    };
    let request = match decode_node_request(message.payload.as_ref()) {
        Ok(request) => request,
        Err(error) => {
            publish_error_response(client, reply, "INVALID_REQUEST", error.to_string()).await?;
            return Ok(());
        }
    };

    let (reply_tx, reply_rx) = oneshot::channel();
    let (response_flushed_tx, response_flushed_rx) = oneshot::channel();
    if tx
        .send(IncomingCommand {
            reply: reply_tx,
            response_flushed: Some(response_flushed_rx),
            stream: None,
            request: IncomingRequest::node(request),
        })
        .await
        .is_err()
    {
        publish_error_response(
            client,
            reply,
            "DAEMON_UNAVAILABLE",
            "daemon command channel closed",
        )
        .await?;
        return Err("daemon command channel closed".into());
    }
    let response = match reply_rx.await {
        Ok(response) => response,
        Err(_) => {
            publish_error_response(
                client,
                reply,
                "DAEMON_RESPONSE_DROPPED",
                "daemon dropped response",
            )
            .await?;
            return Err("daemon dropped response".into());
        }
    };
    let payload = encode_node_response(&response).map_err(|error| error.to_string())?;
    client
        .publish(reply, payload.into())
        .await
        .map_err(|error| format!("publish response: {error}"))?;
    client
        .flush()
        .await
        .map_err(|error| format!("flush response: {error}"))?;
    let _ = response_flushed_tx.send(());
    Ok(())
}

async fn publish_error_response(
    client: async_nats::Client,
    reply: async_nats::Subject,
    code: impl Into<String>,
    message: impl Into<String>,
) -> Result<(), String> {
    let response = ployz_api::DaemonResponse::error(code, message, None);
    let payload = encode_node_response(&response).map_err(|error| error.to_string())?;
    client
        .publish(reply, payload.into())
        .await
        .map_err(|error| format!("publish error response: {error}"))?;
    client
        .flush()
        .await
        .map_err(|error| format!("flush error response: {error}"))
}
