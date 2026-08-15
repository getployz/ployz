use std::{
    collections::HashMap,
    convert::Infallible,
    future::poll_fn,
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use http_body_util::{BodyExt, Full, StreamBody};
use ployz_core::{
    FanoutFailure, FanoutResponse, FramingError, Machine, MachineId, ManagementAddress,
    NameMatches, clear_routing_headers, grpc_frame_length, grpc_frames, resolve_machine_selector,
    resolve_machine_selectors, routing_from_metadata,
};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tonic::{
    Status,
    body::Body,
    codegen::{Service, http},
    metadata::MetadataMap,
    service::Routes,
    transport::{Channel, Endpoint},
};

use crate::corrosion::ReplicatedStore;

#[derive(Debug, Error)]
enum BackendError {
    #[error("remote backend cache lock poisoned")]
    Poisoned,
    #[error("{0}")]
    Endpoint(#[from] tonic::transport::Error),
}

pub use ployz_core::MachineSelectorError as TargetResolutionError;
pub use ployz_core::RoutingRequest;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProxyRoute {
    Local,
    One(Box<Machine>),
    Many(Vec<Machine>),
}

pub fn resolve_route(
    request: RoutingRequest,
    visible: &[Machine],
) -> Result<ProxyRoute, TargetResolutionError> {
    match request {
        RoutingRequest::Local => Ok(ProxyRoute::Local),
        RoutingRequest::One(selector) => match resolve_machine_selector(&selector, visible) {
            NameMatches::One(machine) => Ok(ProxyRoute::One(Box::new(machine.clone()))),
            NameMatches::None => Err(TargetResolutionError::NotFound(vec![selector])),
            NameMatches::Ambiguous(matches) => Err(TargetResolutionError::Ambiguous {
                selector,
                matches: matches.into_iter().map(|machine| machine.id).collect(),
            }),
        },
        RoutingRequest::Many(selectors) => {
            resolve_machine_selectors(visible, &selectors).map(ProxyRoute::Many)
        }
    }
}

#[derive(Clone)]
pub struct MachineProxy {
    local: Routes,
    local_id: MachineId,
    remote_port: u16,
    replicated: Option<ReplicatedStore>,
    // TODO(UT-139): stale remote backends intentionally reconnect until ployzd restarts.
    remote_backends: Arc<Mutex<HashMap<ManagementAddress, Channel>>>,
}

impl MachineProxy {
    #[must_use]
    pub fn new(
        local: Routes,
        local_id: MachineId,
        remote_port: u16,
        replicated: Option<ReplicatedStore>,
    ) -> Self {
        Self {
            local,
            local_id,
            remote_port,
            replicated,
            remote_backends: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn dispatch_with_snapshot(
        &self,
        request: http::Request<Body>,
        visible: &[Machine],
    ) -> http::Response<Body> {
        let routing =
            match routing_from_metadata(&MetadataMap::from_headers(request.headers().clone())) {
                Ok(routing) => routing,
                Err(error) => return Status::invalid_argument(error.to_string()).into_http(),
            };
        self.dispatch(request, routing, visible).await
    }

    async fn dispatch(
        &self,
        request: http::Request<Body>,
        routing: RoutingRequest,
        visible: &[Machine],
    ) -> http::Response<Body> {
        let route = match resolve_route(routing, visible) {
            Ok(route) => route,
            Err(error) => return Status::invalid_argument(error.to_string()).into_http(),
        };
        match route {
            ProxyRoute::Local => self.call_local(request).await,
            ProxyRoute::One(target) => self.call_target(request, &target).await,
            ProxyRoute::Many(targets) => self.call_many(request, &targets).await,
        }
    }

    async fn call_target(
        &self,
        mut request: http::Request<Body>,
        target: &Machine,
    ) -> http::Response<Body> {
        clear_routing_headers(request.headers_mut());
        if target.id == self.local_id {
            return self.call_local(request).await;
        }
        match self.call_remote(request, target.management_address).await {
            Ok(response) => response,
            Err(status) => status.into_http(),
        }
    }

    async fn call_many(
        &self,
        request: http::Request<Body>,
        targets: &[Machine],
    ) -> http::Response<Body> {
        let (parts, body) = request.into_parts();
        // ponytail: fan-out requests are bounded control messages; tee the body if a
        // client-streaming fan-out command is ever added.
        let request_body = match body.collect().await {
            Ok(body) => body.to_bytes(),
            Err(error) => return Status::invalid_argument(error.to_string()).into_http(),
        };
        let (sender, receiver) = mpsc::channel(targets.len().max(1));
        for target in targets {
            let request = request_from_parts(&parts, request_body.clone());
            let proxy = self.clone();
            let target = target.clone();
            let sender = sender.clone();
            tokio::spawn(async move {
                let response = proxy.call_target(request, &target).await;
                stream_target(response, target, sender).await;
            });
        }
        drop(sender);
        grpc_response(receiver)
    }

    async fn call_local(&self, request: http::Request<Body>) -> http::Response<Body> {
        let mut local = self.local.clone();
        poll_fn(|context| Service::<http::Request<Body>>::poll_ready(&mut local, context))
            .await
            .unwrap();
        Service::<http::Request<Body>>::call(&mut local, request)
            .await
            .unwrap()
    }

    async fn call_remote(
        &self,
        request: http::Request<Body>,
        address: ManagementAddress,
    ) -> Result<http::Response<Body>, Status> {
        let mut channel = self
            .remote_backend(address)
            .map_err(|error| Status::internal(error.to_string()))?;
        poll_fn(|context| channel.poll_ready(context))
            .await
            .map_err(|error| Status::unavailable(error.to_string()))?;
        channel
            .call(request)
            .await
            .map_err(|error| Status::unavailable(error.to_string()))
    }

    fn remote_backend(&self, address: ManagementAddress) -> Result<Channel, BackendError> {
        let mut backends = self
            .remote_backends
            .lock()
            .map_err(|_| BackendError::Poisoned)?;
        if let Some(channel) = backends.get(&address) {
            return Ok(channel.clone());
        }
        let endpoint =
            Endpoint::from_shared(format!("http://[{}]:{}", address.0, self.remote_port))?
                .connect_timeout(Duration::from_secs(10));
        let channel = endpoint.connect_lazy();
        backends.insert(address, channel.clone());
        Ok(channel)
    }
}

fn request_from_parts(parts: &http::request::Parts, body: Bytes) -> http::Request<Body> {
    let mut request = http::Request::new(Body::new(Full::new(body)));
    *request.method_mut() = parts.method.clone();
    *request.uri_mut() = parts.uri.clone();
    *request.version_mut() = parts.version;
    *request.headers_mut() = parts.headers.clone();
    request
}

async fn stream_target(
    response: http::Response<Body>,
    target: Machine,
    sender: mpsc::Sender<Bytes>,
) {
    let (parts, body) = response.into_parts();
    if let Some(status) = Status::from_header_map(&parts.headers)
        && status.code() != tonic::Code::Ok
    {
        send_failure(&sender, &target, status).await;
        return;
    }
    let mut body = std::pin::pin!(body);
    let mut buffered = BytesMut::new();
    let mut sent_payload = false;
    while let Some(frame) = body.frame().await {
        let frame = match frame {
            Ok(frame) => frame,
            Err(error) => {
                send_failure(&sender, &target, error).await;
                return;
            }
        };
        match frame.into_data() {
            Ok(data) => {
                buffered.extend_from_slice(&data);
                loop {
                    match take_grpc_frame(&mut buffered) {
                        Ok(Some(frame)) => {
                            sent_payload = true;
                            let envelope =
                                FanoutResponse::success(&target.id, &target.name, frame.to_vec())
                                    .expect("the extracted bytes are one complete gRPC frame");
                            if sender
                                .send(Bytes::from(envelope.encode_grpc_frame()))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            send_failure(&sender, &target, Status::internal(error.to_string()))
                                .await;
                            return;
                        }
                    }
                }
            }
            Err(frame) => {
                if let Ok(trailers) = frame.into_trailers()
                    && let Some(status) = Status::from_header_map(&trailers)
                    && status.code() != tonic::Code::Ok
                {
                    send_failure(&sender, &target, status).await;
                    return;
                }
            }
        }
    }
    if !buffered.is_empty() {
        let error = grpc_frames(&buffered).expect_err("an incomplete buffered frame must fail");
        send_failure(&sender, &target, Status::internal(error.to_string())).await;
    } else if !sent_payload {
        let _ = sender
            .send(Bytes::from(
                FanoutResponse::omission(&target.id, &target.name).encode_grpc_frame(),
            ))
            .await;
    }
}

fn take_grpc_frame(buffered: &mut BytesMut) -> Result<Option<Bytes>, ployz_core::FramingError> {
    let length = match grpc_frame_length(buffered) {
        Ok(length) => length,
        Err(FramingError::TruncatedHeader | FramingError::TruncatedMessage { .. }) => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    Ok(Some(buffered.split_to(length).freeze()))
}

async fn send_failure(sender: &mpsc::Sender<Bytes>, target: &Machine, status: Status) {
    let _ = sender
        .send(Bytes::from(
            FanoutResponse::failure(&target.id, &target.name, status_failure(status))
                .encode_grpc_frame(),
        ))
        .await;
}

fn status_failure(status: Status) -> FanoutFailure {
    FanoutFailure {
        code: status.code() as u32,
        message: status.message().to_owned(),
        details: status.details().to_vec(),
    }
}

fn grpc_response(receiver: mpsc::Receiver<Bytes>) -> http::Response<Body> {
    let stream = ReceiverStream::new(receiver)
        .map(|bytes| Ok::<_, Infallible>(http_body::Frame::data(bytes)));
    let mut response = http::Response::new(Body::new(StreamBody::new(stream)));
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/grpc"),
    );
    response
        .headers_mut()
        .insert("grpc-status", http::HeaderValue::from_static("0"));
    response
}

impl Service<http::Request<Body>> for MachineProxy {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = std::pin::Pin<
        Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>,
    >;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let proxy = self.clone();
        Box::pin(async move {
            let routing = match routing_from_metadata(&MetadataMap::from_headers(
                request.headers().clone(),
            )) {
                Ok(routing) => routing,
                Err(error) => {
                    return Ok(Status::invalid_argument(error.to_string()).into_http());
                }
            };
            if routing == RoutingRequest::Local {
                return Ok(proxy.dispatch(request, routing, &[]).await);
            }
            let visible = match &proxy.replicated {
                Some(store) => match store.machines().await {
                    Ok(snapshot) => snapshot.observations,
                    Err(error) => {
                        return Ok(Status::unavailable(error.to_string()).into_http());
                    }
                },
                None => Vec::new(),
            };
            Ok(proxy.dispatch(request, routing, &visible).await)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv6Addr;

    use ployz_core::{MachineId, ManagementAddress};
    use tonic::service::Routes;

    use super::MachineProxy;

    #[tokio::test]
    async fn remote_backend_survives_disappearance_from_later_snapshots() {
        let proxy = MachineProxy::new(Routes::default(), MachineId::random(), 51000, None);
        proxy
            .remote_backend(ManagementAddress(Ipv6Addr::LOCALHOST))
            .unwrap();
        assert_eq!(proxy.remote_backends.lock().unwrap().len(), 1);

        let request = tonic::codegen::http::Request::builder()
            .header("machine", MachineId::random().as_str())
            .body(tonic::body::Body::empty())
            .unwrap();
        let response = proxy.dispatch_with_snapshot(request, &[]).await;
        assert_eq!(
            tonic::Status::from_header_map(response.headers())
                .unwrap()
                .code(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(proxy.remote_backends.lock().unwrap().len(), 1);
    }
}
