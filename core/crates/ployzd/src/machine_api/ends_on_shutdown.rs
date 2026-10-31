//! Daemon shutdown ends the Machine API's open-ended streams at once.

use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use http_body::Frame;
use http_body_util::{BodyExt, Full};
use ployz_core::{OpaquePayload, RpcRequestBody, op};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};
use tonic::{
    body::Body,
    codegen::{Service, http},
};

/// Serves `S`, ending its open-ended streams with UNAVAILABLE once `shutdown` fires.
///
/// Exec, RuntimeWatch and a following TailLogs end only when their caller leaves. The
/// daemon's server drain would otherwise wait them out until it gives up and aborts
/// every server, dropping the management endpoint unclosed (#3699). Their callers
/// already reconnect on UNAVAILABLE, the status a stopped daemon gives too. Every
/// other call, a bounded TailLogs read, Build and Leave's reply among them, drains as
/// before.
#[derive(Clone)]
pub struct EndsOnShutdown<S> {
    inner: S,
    shutdown: CancellationToken,
}

impl<S> EndsOnShutdown<S> {
    #[must_use]
    pub fn new(inner: S, shutdown: CancellationToken) -> Self {
        Self { inner, shutdown }
    }
}

impl<S> Service<http::Request<Body>> for EndsOnShutdown<S>
where
    S: Service<http::Request<Body>, Response = http::Response<Body>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let path = request.uri().path();
        let shutdown = self.shutdown.clone();
        if path == op::TailLogs::PATH {
            // The readied service answers; its clone takes the next call.
            let ready = self.inner.clone();
            let mut inner = std::mem::replace(&mut self.inner, ready);
            return Box::pin(async move {
                let (parts, body) = request.into_parts();
                let frame = match body.collect().await {
                    Ok(collected) => collected.to_bytes(),
                    Err(status) => return Ok(status.into_http()),
                };
                let follows = tail_follows(&frame);
                let request = http::Request::from_parts(parts, Body::new(Full::new(frame)));
                let response = inner.call(request);
                if follows {
                    until_shutdown(response, shutdown).await
                } else {
                    response.await
                }
            });
        }
        let open_ended = path == op::Exec::PATH || path == op::RuntimeWatch::PATH;
        let response = self.inner.call(request);
        if open_ended {
            Box::pin(until_shutdown(response, shutdown))
        } else {
            Box::pin(response)
        }
    }
}

async fn until_shutdown(
    response: impl Future<Output = Result<http::Response<Body>, Infallible>>,
    shutdown: CancellationToken,
) -> Result<http::Response<Body>, Infallible> {
    tokio::select! {
        response = response => Ok(response?.map(|body| {
            Body::new(UntilShutdown {
                body,
                shutdown: Box::pin(shutdown.cancelled_owned()),
                ended: false,
            })
        })),
        () = shutdown.cancelled() => Ok(shutting_down().into_http()),
    }
}

/// Whether a TailLogs request's single gRPC frame asks to follow. A frame this cannot
/// read counts as following; the handler refuses a malformed one at once anyway.
fn tail_follows(frame: &[u8]) -> bool {
    // Uncompressed flag, then a four-byte length; no client compresses requests.
    let Some(message) = frame.strip_prefix(&[0]).and_then(|rest| rest.get(4..)) else {
        return true;
    };
    let Ok(payload) = <OpaquePayload as prost::Message>::decode(message) else {
        return true;
    };
    match payload.decode_request().map(|request| request.body) {
        Ok(RpcRequestBody::TailLogs(request)) => request.options.follow,
        _ => true,
    }
}

fn shutting_down() -> tonic::Status {
    tonic::Status::unavailable("daemon shutting down")
}

/// A response body that ends with [`shutting_down`] trailers once the daemon shuts down.
struct UntilShutdown {
    body: Body,
    shutdown: Pin<Box<WaitForCancellationFutureOwned>>,
    ended: bool,
}

impl http_body::Body for UntilShutdown {
    type Data = Bytes;
    type Error = tonic::Status;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        if self.ended {
            return Poll::Ready(None);
        }
        if self.shutdown.as_mut().poll(cx).is_ready() {
            self.ended = true;
            let mut trailers = http::HeaderMap::new();
            shutting_down().add_header(&mut trailers)?;
            return Poll::Ready(Some(Ok(Frame::trailers(trailers))));
        }
        Pin::new(&mut self.body).poll_frame(cx)
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{LiveLogTarget, LogsOptions, MachineLogService, TailLogsRequest};

    use super::*;

    fn tail(follow: bool) -> Vec<u8> {
        let message = prost::Message::encode_to_vec(
            &op::TailLogs::into_request(TailLogsRequest {
                target: LiveLogTarget::Machine(MachineLogService::Docker),
                options: LogsOptions {
                    follow,
                    tail: 10,
                    since_nanos: None,
                    until_unix_seconds: None,
                },
            })
            .encode()
            .unwrap(),
        );
        let mut frame = vec![0];
        frame.extend_from_slice(&u32::try_from(message.len()).unwrap().to_be_bytes());
        frame.extend_from_slice(&message);
        frame
    }

    #[test]
    fn only_a_following_tail_is_open_ended() {
        assert!(tail_follows(&tail(true)));
        assert!(!tail_follows(&tail(false)));
        let mut compressed = tail(false);
        compressed.splice(..1, [1]);
        assert!(
            tail_follows(&compressed),
            "an unreadable frame counts as following"
        );
        assert!(tail_follows(&[]));
    }
}
