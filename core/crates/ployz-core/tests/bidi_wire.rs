#![cfg(not(target_arch = "wasm32"))]

use std::convert::Infallible;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use ployz_core::{MachineRpcClient, OpaquePayload};
use tonic::body::Body;
use tonic::codegen::Body as _;
use tonic::codegen::http::{Request, Response};
use tonic::codegen::{Service, tokio_stream};

#[derive(Debug, Default, PartialEq)]
struct Sent {
    path: String,
    content_type: String,
    body: Vec<u8>,
}

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Sent>>);

impl Service<Request<Body>> for Recorder {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Response<Body>, Infallible>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        let sent = self.0.clone();
        Box::pin(async move {
            let path = request.uri().path().to_owned();
            let content_type = request.headers()["content-type"]
                .to_str()
                .unwrap()
                .to_owned();
            let mut body = request.into_body();
            let mut bytes = Vec::new();
            while let Some(frame) = poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
                if let Ok(data) = frame.unwrap().into_data() {
                    bytes.extend_from_slice(&data);
                }
            }
            *sent.lock().unwrap() = Sent {
                path,
                content_type,
                body: bytes,
            };
            let reply = String::from("\0\0\0\0\x09\x0a\x07{\"r\":2}");
            Ok(Response::builder()
                .header("content-type", "application/grpc")
                .body(Body::new(reply))
                .unwrap())
        })
    }
}

fn frames() -> impl tokio_stream::Stream<Item = OpaquePayload> + Send + 'static {
    tokio_stream::iter([
        OpaquePayload::new(b"{\"a\":1}".to_vec()),
        OpaquePayload::new(b"{}".to_vec()),
    ])
}

fn sent(path: &str) -> Sent {
    Sent {
        path: path.to_owned(),
        content_type: "application/grpc".to_owned(),
        body: b"\0\0\0\0\x09\x0a\x07{\"a\":1}\0\0\0\0\x04\x0a\x02{}".to_vec(),
    }
}

/// Exec and Build are bidirectional streams of `OpaquePayload`. A daemon
/// from any 0.x release must keep reading these exact bytes on these exact paths.
#[tokio::test]
async fn exec_and_build_keep_their_bidi_wire_bytes() {
    let recorder = Recorder::default();
    let mut client = MachineRpcClient::new(recorder.clone());

    let mut replies = client.exec(frames()).await.unwrap().into_inner();
    let reply = replies.message().await.unwrap().unwrap();
    assert_eq!(reply.json, b"{\"r\":2}");
    assert_eq!(
        std::mem::take(&mut *recorder.0.lock().unwrap()),
        sent("/ployz.rpc.v1.MachineRpc/Exec")
    );

    let mut replies = client.build(frames()).await.unwrap().into_inner();
    let reply = replies.message().await.unwrap().unwrap();
    assert_eq!(reply.json, b"{\"r\":2}");
    assert_eq!(
        std::mem::take(&mut *recorder.0.lock().unwrap()),
        sent("/ployz.rpc.v1.MachineRpc/Build")
    );
}
