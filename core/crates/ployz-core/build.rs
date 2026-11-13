include!("src/rpc_catalog.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/rpc_catalog.rs");
    macro_rules! build_service {
        (
            package $package:literal
            unary { $($unary_variant:ident: ($unary_name:ident, $unary_request:ty, $unary_response:ty, $unary_capability:ident, $unary_capability_name:literal, $unary_advertisement:ident, $unary_scope:ident $(, $($unary_more:tt)*)?),)+ }
            server_streaming { $($stream_variant:ident: ($stream_name:ident, $stream_request:ty, $stream_capability:ident, $stream_capability_name:literal, $stream_advertisement:ident, $stream_scope:ident $(, $($stream_more:tt)*)?),)+ }
            bidi_streaming { $($bidi_variant:ident: ($bidi_method:ident, $bidi_capability:ident, $bidi_capability_name:literal, $bidi_advertisement:ident, $bidi_scope:ident $(, $($bidi_more:tt)*)?),)+ }
            capabilities { $($(#[$extra_meta:meta])* $extra_capability:ident: ($extra_capability_name:literal, $extra_advertisement:ident),)+ }
        ) => {{
            let mut service = tonic_build::manual::Service::builder()
                .name("MachineRpc")
                .package($package)
                .comment("Machine control RPCs with schema-blind protobuf envelopes.");
            $(service = service.method(method(stringify!($unary_name), stringify!($unary_variant)));)+
            $(service = service.method(streaming_method(stringify!($stream_name), stringify!($stream_variant)));)+
            $(service = service.method(bidi_method(stringify!($bidi_method), stringify!($bidi_variant)));)+
            service
        }};
    }
    let machine_rpc = rpc_catalog!(build_service).build();
    tonic_build::manual::Builder::new().compile(&[machine_rpc]);
}

fn bidi_method(name: &str, route: &str) -> tonic_build::manual::Method {
    tonic_build::manual::Method::builder()
        .name(name)
        .route_name(route)
        .input_type("crate::rpc::OpaquePayload")
        .output_type("crate::rpc::OpaquePayload")
        .codec_path("tonic::codec::ProstCodec")
        .client_streaming()
        .server_streaming()
        .build()
}

fn streaming_method(name: &str, route: &str) -> tonic_build::manual::Method {
    tonic_build::manual::Method::builder()
        .name(name)
        .route_name(route)
        .input_type("crate::rpc::OpaquePayload")
        .output_type("crate::rpc::OpaquePayload")
        .codec_path("tonic::codec::ProstCodec")
        .server_streaming()
        .build()
}

fn method(name: &str, route: &str) -> tonic_build::manual::Method {
    tonic_build::manual::Method::builder()
        .name(name)
        .route_name(route)
        .input_type("crate::rpc::OpaquePayload")
        .output_type("crate::rpc::OpaquePayload")
        .codec_path("tonic::codec::ProstCodec")
        .build()
}
