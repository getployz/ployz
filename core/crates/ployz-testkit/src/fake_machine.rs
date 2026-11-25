//! The default Machine API fake for tests.
//!
//! A fixture implements [`FakeMachine`] for only the RPCs it exercises and serves
//! [`Fake`] of itself. Every other RPC answers `Unimplemented`, so a new catalog row
//! compiles against every fixture without an edit.

#![expect(
    clippy::result_large_err,
    reason = "every handler answers tonic's Status, which tonic sizes"
)]

use std::{collections::BTreeMap, pin::Pin, sync::Mutex};

use ployz_core::{
    ContainerAddress, ContainerObservation, MachineId, MachineRpc, OpaquePayload, ProxyRoutes,
    QualifiedService, RoutesRead, RpcResponse,
};
use tokio_stream::Stream;
use tonic::{Request, Response, Status, Streaming};

/// The response stream of every streaming RPC on a [`FakeMachine`].
pub type FakeStream = Pin<Box<dyn Stream<Item = Result<OpaquePayload, Status>> + Send>>;

/// `response` as a unary RPC's answer.
///
/// # Errors
/// Internal when the response does not encode.
pub fn reply(response: impl Into<RpcResponse>) -> Result<Response<OpaquePayload>, Status> {
    response
        .into()
        .encode()
        .map(Response::new)
        .map_err(|error| Status::internal(error.to_string()))
}

/// The answer to an RPC a fixture serves only in some setups.
///
/// # Errors
/// Always `Unimplemented`.
pub fn unused<T>() -> Result<T, Status> {
    Err(Status::unimplemented("unused"))
}

/// The Machine a routed request targets by ID.
///
/// # Errors
/// Invalid argument when the request carries no Machine ID target.
pub fn target<T>(request: &Request<T>) -> Result<MachineId, Status> {
    let value = request
        .metadata()
        .get("machine")
        .ok_or_else(|| Status::invalid_argument("missing machine target"))?
        .to_str()
        .map_err(|_| Status::invalid_argument("machine target is not utf-8"))?;
    MachineId::parse(value).map_err(|error| Status::invalid_argument(error.to_string()))
}

/// Each Machine's Ingress Proxy, routing by the real proxy rule from what it last routed.
#[derive(Default)]
pub struct Proxies {
    served: Mutex<BTreeMap<MachineId, ProxyRoutes>>,
}

impl Proxies {
    /// What the proxy on each of `machines` routes for `service` once it sees `copies`,
    /// every copy addressed.
    ///
    /// # Panics
    /// When the lock is poisoned or an address does not fit a copy.
    pub fn read(
        &self,
        service: &QualifiedService,
        machines: impl IntoIterator<Item = MachineId>,
        copies: impl IntoIterator<Item = ContainerObservation>,
    ) -> RoutesRead {
        let siblings = ployz_core::service_containers(
            copies
                .into_iter()
                .filter(|copy| copy.identity() == *service)
                .map(|mut copy| {
                    copy.try_update(|parts| {
                        parts.address =
                            parts
                                .address
                                .or(Some(ContainerAddress(std::net::Ipv4Addr::new(
                                    10, 210, 1, 2,
                                ))));
                    })
                    .expect("an address fits any copy");
                    copy
                }),
        );
        let mut served = self.served.lock().expect("proxy state lock");
        RoutesRead {
            routes: machines
                .into_iter()
                .map(|machine| {
                    let current = served.entry(machine).or_default();
                    let next = ployz_core::proxy_routes(&siblings, |_| false, current);
                    let route = next.get(service).cloned().unwrap_or_default();
                    let listed = route
                        .iter()
                        .map(|serving| serving.as_observation().container_id)
                        .collect();
                    if next.contains_key(service) {
                        current.insert(service.clone(), route);
                    } else {
                        current.remove(service);
                    }
                    (machine, Some(listed))
                })
                .collect(),
            complete: true,
            ..RoutesRead::default()
        }
    }
}

/// Serves a [`FakeMachine`] as the Machine API.
pub struct Fake<T>(pub T);

macro_rules! fake_machine {
    (
        package $package:literal
        unary {
            $($unary_variant:ident: ($unary_method:ident, $unary_request:ty, $unary_response:ty, $unary_capability:ident, $unary_capability_name:literal, $unary_advertisement:ident, $unary_scope:ident $(, $($unary_more:tt)*)?),)+
        }
        server_streaming {
            $($stream_variant:ident: ($stream_method:ident, $stream_request:ty, $stream_capability:ident, $stream_capability_name:literal, $stream_advertisement:ident, $stream_scope:ident $(, $($stream_more:tt)*)?),)+
        }
        bidi_streaming {
            $($bidi_variant:ident: ($bidi_method:ident, $bidi_capability:ident, $bidi_capability_name:literal, $bidi_advertisement:ident, $bidi_scope:ident $(, $($bidi_more:tt)*)?),)+
        }
        capabilities {
            $($(#[$extra_meta:meta])* $extra_capability:ident: ($extra_capability_name:literal, $extra_advertisement:ident),)+
        }
    ) => {
        /// A Machine API whose RPCs answer `Unimplemented` unless overridden.
        #[tonic::async_trait]
        pub trait FakeMachine: Send + Sync + 'static {
            $(
                async fn $unary_method(
                    &self,
                    _request: Request<OpaquePayload>,
                ) -> Result<Response<OpaquePayload>, Status> {
                    Err(Status::unimplemented(concat!(stringify!($unary_variant), " is not used by this fixture")))
                }
            )+
            $(
                async fn $stream_method(
                    &self,
                    _request: Request<OpaquePayload>,
                ) -> Result<Response<FakeStream>, Status> {
                    Err(Status::unimplemented(concat!(stringify!($stream_variant), " is not used by this fixture")))
                }
            )+
            $(
                async fn $bidi_method(
                    &self,
                    _request: Request<Streaming<OpaquePayload>>,
                ) -> Result<Response<FakeStream>, Status> {
                    Err(Status::unimplemented(concat!(stringify!($bidi_variant), " is not used by this fixture")))
                }
            )+
        }

        #[tonic::async_trait]
        impl<T: FakeMachine> MachineRpc for Fake<T> {
            type ExecStream = FakeStream;
            type BuildStream = FakeStream;
            type BuildFreeStream = FakeStream;
            type TailLogsStream = FakeStream;
            type RuntimeWatchStream = FakeStream;

            $(
                async fn $unary_method(
                    &self,
                    request: Request<OpaquePayload>,
                ) -> Result<Response<OpaquePayload>, Status> {
                    self.0.$unary_method(request).await
                }
            )+
            $(
                async fn $stream_method(
                    &self,
                    request: Request<OpaquePayload>,
                ) -> Result<Response<FakeStream>, Status> {
                    self.0.$stream_method(request).await
                }
            )+
            $(
                async fn $bidi_method(
                    &self,
                    request: Request<Streaming<OpaquePayload>>,
                ) -> Result<Response<FakeStream>, Status> {
                    self.0.$bidi_method(request).await
                }
            )+
        }
    };
}

ployz_core::rpc_catalog!(fake_machine);
