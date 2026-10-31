//! Daemon shutdown ends the Machine API's open-ended streams at once.

use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use http_body::Frame;
use ployz_core::op;
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};
use tonic::{
    body::Body,
    codegen::{Service, http},
};

/// Serves `S`, ending its open-ended streams with UNAVAILABLE once `shutdown` fires.
///
/// Exec, TailLogs and RuntimeWatch end only when their caller leaves. The daemon's
/// server drain would otherwise wait them out until it gives up and aborts every
/// server, dropping the management endpoint unclosed (#3699). Their callers already
/// reconnect on UNAVAILABLE, the status a stopped daemon gives too. Every other call,
/// Build and Leave's reply among them, drains as before.
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
    S: Service<http::Request<Body>, Response = http::Response<Body>, Error = Infallible>,
    S::Future: Send + 'static,
{
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let open_ended = open_ended(request.uri().path());
        let response = self.inner.call(request);
        if !open_ended {
            return Box::pin(response);
        }
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
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
        })
    }
}

fn open_ended(path: &str) -> bool {
    [op::Exec::PATH, op::TailLogs::PATH, op::RuntimeWatch::PATH].contains(&path)
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
    use ployz_core::Rpc;

    use super::*;

    #[test]
    fn only_streams_without_an_end_are_open_ended() {
        for path in [op::Exec::PATH, op::TailLogs::PATH, op::RuntimeWatch::PATH] {
            assert!(open_ended(path), "{path}");
        }
        for path in [op::Build::PATH, op::LogHistory::PATH, op::Leave::PATH] {
            assert!(!open_ended(path), "{path}");
        }
    }
}
