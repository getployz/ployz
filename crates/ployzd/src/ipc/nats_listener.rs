use futures_util::StreamExt;
use ployz_nats::coord::rpc::{decode_daemon_request, encode_daemon_response};
use ployz_runtime_api::RuntimeHandle;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::listener::IncomingCommand;

const RESUBSCRIBE_DELAY: Duration = Duration::from_secs(1);

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
    tx: mpsc::Sender<IncomingCommand>,
) -> Result<NatsListenerHandle, String> {
    let mut subscriber = client
        .subscribe(subject.clone())
        .await
        .map_err(|error| format!("subscribe {subject}: {error}"))?;
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        info!(%subject, "nats node rpc listener subscribed");
        loop {
            tokio::select! {
                _ = task_cancel.cancelled() => {
                    info!(%subject, "nats node rpc listener shutting down");
                    break;
                }
                next = subscriber.next() => {
                    let Some(message) = next else {
                        warn!(%subject, "nats node rpc subscription closed; resubscribing");
                        subscriber = match resubscribe(&client, &subject, &task_cancel).await {
                            Some(subscriber) => subscriber,
                            None => break,
                        };
                        continue;
                    };
                    let client = client.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
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
    cancel: &CancellationToken,
) -> Option<async_nats::Subscriber> {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return None,
            _ = tokio::time::sleep(RESUBSCRIBE_DELAY) => {}
        }
        match client.subscribe(subject.to_string()).await {
            Ok(subscriber) => {
                info!(%subject, "nats node rpc listener resubscribed");
                return Some(subscriber);
            }
            Err(error) => {
                warn!(%subject, %error, "nats node rpc resubscribe failed");
            }
        }
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
        .map_err(|error| format!("publish error response: {error}"))
}
