//! The default Machine API fake for tests.
//!
//! A fixture implements [`FakeMachine`] for only the RPCs it exercises and serves
//! [`Fake`] of itself. Every other RPC answers `Unimplemented`, so a new catalog row
//! compiles against every fixture without an edit.

use std::pin::Pin;

use ployz_core::{MachineRpc, OpaquePayload};
use tokio_stream::Stream;
use tonic::{Request, Response, Status, Streaming};

/// The response stream of every streaming RPC on a [`FakeMachine`].
pub type FakeStream = Pin<Box<dyn Stream<Item = Result<OpaquePayload, Status>> + Send>>;

/// Serves a [`FakeMachine`] as the Machine API.
pub struct Fake<T>(pub T);

macro_rules! fake_machine {
    (
        package $package:literal
        unary {
            $($unary_variant:ident: ($unary_method:ident, $unary_route:literal, $unary_request:ty, $unary_command:literal, $unary_response:ty, $unary_capability:ident, $unary_capability_name:literal, $unary_advertisement:ident),)+
        }
        server_streaming {
            $($stream_variant:ident: ($stream_method:ident, $stream_route:literal, $stream_request:ty, $stream_command:literal, $stream_capability:ident, $stream_capability_name:literal, $stream_advertisement:ident),)+
        }
        bidi_streaming {
            $($bidi_variant:ident: ($bidi_method:ident, $bidi_route:literal, $bidi_command:literal, $bidi_capability:ident, $bidi_capability_name:literal, $bidi_advertisement:ident),)+
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
                    Err(Status::unimplemented(concat!($unary_route, " is not used by this fixture")))
                }
            )+
            $(
                async fn $stream_method(
                    &self,
                    _request: Request<OpaquePayload>,
                ) -> Result<Response<FakeStream>, Status> {
                    Err(Status::unimplemented(concat!($stream_route, " is not used by this fixture")))
                }
            )+
            $(
                async fn $bidi_method(
                    &self,
                    _request: Request<Streaming<OpaquePayload>>,
                ) -> Result<Response<FakeStream>, Status> {
                    Err(Status::unimplemented(concat!($bidi_route, " is not used by this fixture")))
                }
            )+
        }

        #[tonic::async_trait]
        impl<T: FakeMachine> MachineRpc for Fake<T> {
            type ExecStream = FakeStream;
            type BuildStream = FakeStream;
            type LogHistoryStream = FakeStream;
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
