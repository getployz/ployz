use futures_util::StreamExt;
use ployz_nats::coord::rpc::{decode_daemon_request, encode_daemon_response};
use ployz_runtime_api::RuntimeHandle;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::listener::IncomingCommand;

const RESUBSCRIBE_DELAY: Duration = Duration::from_secs(1);
const MAX_IN_FLIGHT_COMMANDS: usize = 64;
pub const NATS_NODE_RPC_HEALTH_FILE: &str = "nats-node-rpc-health.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NatsNodeRpcHealth {
    pub healthy: bool,
    pub updated_at_unix_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_since_unix_secs: Option<u64>,
    pub consecutive_failures: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

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
    write_health(&health_path, healthy_health()).await;
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        let permits = Arc::new(Semaphore::new(MAX_IN_FLIGHT_COMMANDS));
        let mut consecutive_failures = 0_u64;
        let mut stale_since_unix_secs = None;
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
                        record_unhealthy(
                            &health_path,
                            &mut consecutive_failures,
                            &mut stale_since_unix_secs,
                            "nats node rpc subscription closed",
                        ).await;
                        subscriber = match resubscribe(
                            &client,
                            &subject,
                            &queue_group,
                            &task_cancel,
                            &health_path,
                            &mut consecutive_failures,
                            &mut stale_since_unix_secs,
                        ).await {
                            Some(subscriber) => {
                                consecutive_failures = 0;
                                stale_since_unix_secs = None;
                                write_health(&health_path, healthy_health()).await;
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
                    tokio::spawn(async move {
                        let _permit = permit;
                        if let Err(error) = handle_message(client, message, tx).await {
                            warn!(%error, "nats node rpc message failed");
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
    health_path: &PathBuf,
    consecutive_failures: &mut u64,
    stale_since_unix_secs: &mut Option<u64>,
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
                record_unhealthy(
                    health_path,
                    consecutive_failures,
                    stale_since_unix_secs,
                    format!("nats node rpc resubscribe failed: {error}"),
                )
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

async fn record_unhealthy(
    path: &PathBuf,
    consecutive_failures: &mut u64,
    stale_since_unix_secs: &mut Option<u64>,
    error: impl Into<String>,
) {
    *consecutive_failures += 1;
    let now = unix_secs();
    let stale_since = *stale_since_unix_secs.get_or_insert(now);
    write_health(
        path,
        NatsNodeRpcHealth {
            healthy: false,
            updated_at_unix_secs: now,
            stale_since_unix_secs: Some(stale_since),
            consecutive_failures: *consecutive_failures,
            last_error: Some(error.into()),
        },
    )
    .await;
}

fn healthy_health() -> NatsNodeRpcHealth {
    NatsNodeRpcHealth {
        healthy: true,
        updated_at_unix_secs: unix_secs(),
        stale_since_unix_secs: None,
        consecutive_failures: 0,
        last_error: None,
    }
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
    use super::{NatsNodeRpcHealth, healthy_health, load_health, record_unhealthy, write_health};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    #[tokio::test]
    async fn node_rpc_health_roundtrip() {
        let path = temp_path("node-rpc-health-roundtrip").join("health.json");
        let health = NatsNodeRpcHealth {
            healthy: false,
            updated_at_unix_secs: 1_777_646_000,
            stale_since_unix_secs: Some(1_777_646_000),
            consecutive_failures: 2,
            last_error: Some(String::from("subscription closed")),
        };

        write_health(&path, health.clone()).await;

        let loaded = load_health(path).await.expect("load health");
        assert_eq!(loaded, health);
    }

    #[tokio::test]
    async fn node_rpc_unhealthy_keeps_original_stale_since() {
        let path = temp_path("node-rpc-health-stale").join("health.json");
        let mut failures = 0;
        let mut stale_since = None;

        record_unhealthy(&path, &mut failures, &mut stale_since, "first").await;
        let first = load_health(path.clone()).await.expect("load first health");
        record_unhealthy(&path, &mut failures, &mut stale_since, "second").await;
        let second = load_health(path).await.expect("load second health");

        assert_eq!(first.stale_since_unix_secs, second.stale_since_unix_secs);
        assert_eq!(second.consecutive_failures, 2);
        assert_eq!(second.last_error.as_deref(), Some("second"));
    }

    #[test]
    fn node_rpc_healthy_state_is_fresh() {
        let health = healthy_health();

        assert!(health.healthy);
        assert_eq!(health.consecutive_failures, 0);
        assert_eq!(health.stale_since_unix_secs, None);
        assert_eq!(health.last_error, None);
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
        return Err("request missing reply subject".into());
    };
    let request = match decode_daemon_request(message.payload.as_ref()) {
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
            request,
            reply: reply_tx,
            response_flushed: Some(response_flushed_rx),
            stream: None,
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
        return Ok(());
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
            return Ok(());
        }
    };
    let payload = encode_daemon_response(&response).map_err(|error| error.to_string())?;
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
    let response = ployz_api::DaemonResponse {
        ok: false,
        code: code.into(),
        message: message.into(),
        payload: None,
    };
    let payload = encode_daemon_response(&response).map_err(|error| error.to_string())?;
    client
        .publish(reply, payload.into())
        .await
        .map_err(|error| format!("publish error response: {error}"))?;
    client
        .flush()
        .await
        .map_err(|error| format!("flush error response: {error}"))
}
