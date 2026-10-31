//! Daemon shutdown ends the Machine API's open-ended streams at once.

use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::{Bytes, BytesMut};
use http_body::Frame;
use http_body_util::BodyExt;
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
///
/// To tell a following tail from a bounded one, the wrapper peeks at most one small
/// frame of a TailLogs request, then hands `S` every byte unchanged, so tonic checks and
/// refuses the request exactly as without it.
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
                let (follows, body) = peek_tail(body).await;
                let request = http::Request::from_parts(parts, body);
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

/// The most of a TailLogs request read to find its follow flag. A real one is a few
/// hundred bytes; a larger first frame counts as following.
const TAIL_PEEK_LIMIT: usize = 64 * 1024;

/// Reads `body` until its first gRPC frame is whole or can't be peeked. Returns whether
/// that frame asks to follow, and a body replaying every frame read, then the rest.
async fn peek_tail(mut body: Body) -> (bool, Body) {
    let mut prefix = BytesMut::new();
    let mut held = None;
    let mut ended = false;
    let follows = loop {
        if let Some(follows) = first_frame_follows(&prefix) {
            break follows;
        }
        match body.frame().await {
            Some(Ok(frame)) => match frame.into_data() {
                Ok(data) => prefix.extend_from_slice(&data),
                Err(frame) => {
                    held = Some(Ok(frame));
                    break true;
                }
            },
            Some(Err(status)) => {
                held = Some(Err(status));
                break true;
            }
            None => {
                ended = true;
                break true;
            }
        }
    };
    let replayed = Replayed {
        prefix: Some(prefix.freeze()).filter(|prefix| !prefix.is_empty()),
        held,
        rest: (!ended).then_some(body),
    };
    (follows, Body::new(replayed))
}

/// `Some` once `prefix` holds the first frame whole, or a header too large or
/// compressed to peek past; such a frame counts as following.
fn first_frame_follows(prefix: &[u8]) -> Option<bool> {
    let (&flag, rest) = prefix.split_first()?;
    let length: [u8; 4] = rest.get(..4)?.try_into().ok()?;
    let length = usize::try_from(u32::from_be_bytes(length)).ok()?;
    // No client compresses requests.
    if flag != 0 || length > TAIL_PEEK_LIMIT {
        return Some(true);
    }
    Some(tail_follows(rest.get(4..4 + length)?))
}

/// Whether a TailLogs request message asks to follow. One this cannot read counts as
/// following; the handler refuses a malformed one at once anyway.
fn tail_follows(message: &[u8]) -> bool {
    let Ok(payload) = <OpaquePayload as prost::Message>::decode(message) else {
        return true;
    };
    match payload.decode_request().map(|request| request.body) {
        Ok(RpcRequestBody::TailLogs(request)) => request.options.follow,
        _ => true,
    }
}

/// A request body replaying what [`peek_tail`] read, then the rest unread.
struct Replayed {
    prefix: Option<Bytes>,
    held: Option<Result<Frame<Bytes>, tonic::Status>>,
    rest: Option<Body>,
}

impl http_body::Body for Replayed {
    type Data = Bytes;
    type Error = tonic::Status;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        if let Some(prefix) = self.prefix.take() {
            return Poll::Ready(Some(Ok(Frame::data(prefix))));
        }
        if let Some(held) = self.held.take() {
            return Poll::Ready(Some(held));
        }
        match self.rest.as_mut() {
            Some(rest) => Pin::new(rest).poll_frame(cx),
            None => Poll::Ready(None),
        }
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
    use std::{sync::Arc, time::Duration};

    use futures_util::StreamExt;
    use ployz_core::{LiveLogTarget, LogsOptions, MachineLogService, TailLogsRequest};
    use tokio::sync::Mutex;

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
        assert_eq!(first_frame_follows(&tail(true)), Some(true));
        assert_eq!(first_frame_follows(&tail(false)), Some(false));
        let whole = tail(false);
        assert_eq!(
            first_frame_follows(whole.get(..whole.len() - 1).unwrap()),
            None
        );
        assert_eq!(first_frame_follows(&[0, 0]), None);
        let mut compressed = tail(false);
        compressed.splice(..1, [1]);
        assert_eq!(first_frame_follows(&compressed), Some(true));
    }

    /// Keeps the body each request hands it.
    #[derive(Clone, Default)]
    struct Received(Arc<Mutex<Vec<u8>>>);

    impl Service<http::Request<Body>> for Received {
        type Response = http::Response<Body>;
        type Error = Infallible;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Infallible>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: http::Request<Body>) -> Self::Future {
            let received = Arc::clone(&self.0);
            Box::pin(async move {
                let body = request.into_body().collect().await.unwrap().to_bytes();
                received.lock().await.extend_from_slice(&body);
                Ok(http::Response::new(Body::empty()))
            })
        }
    }

    fn chunked(bytes: &[u8]) -> Body {
        let chunks: Vec<Result<Frame<Bytes>, Box<tonic::Status>>> = bytes
            .chunks(7)
            .map(|chunk| Ok(Frame::data(Bytes::copy_from_slice(chunk))))
            .collect();
        Body::new(http_body_util::StreamBody::new(futures_util::stream::iter(
            chunks,
        )))
    }

    #[tokio::test]
    async fn the_peeked_request_reaches_the_service_unchanged() {
        let frame = tail(false);
        let received = Received::default();
        let request = http::Request::builder()
            .uri(op::TailLogs::PATH)
            .body(chunked(&frame))
            .unwrap();
        EndsOnShutdown::new(received.clone(), CancellationToken::new())
            .call(request)
            .await
            .unwrap();
        assert_eq!(*received.0.lock().await, frame);
    }

    #[tokio::test]
    async fn a_header_too_large_to_peek_is_passed_on_unread() {
        let mut header = vec![0];
        header.extend_from_slice(&u32::MAX.to_be_bytes());
        let body = Body::new(http_body_util::StreamBody::new(
            futures_util::stream::iter([Ok::<_, Box<tonic::Status>>(Frame::data(Bytes::from(
                header.clone(),
            )))])
            .chain(futures_util::stream::pending()),
        ));
        let (follows, mut replayed) = tokio::time::timeout(Duration::from_secs(5), peek_tail(body))
            .await
            .expect("an oversized header is decided without reading on");
        assert!(follows);
        let first = replayed
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap();
        assert_eq!(first, header);
    }
}
