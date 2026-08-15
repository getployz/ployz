include!("src/rpc_catalog.rs");

fn main() {
    macro_rules! build_service {
        (
            package $package:literal
            unary { $($unary_variant:ident: ($unary_name:ident, $unary_route:literal, $unary_request:ty, $unary_command:literal, $unary_response:ty),)+ }
            server_streaming { $($stream_variant:ident: ($stream_name:ident, $stream_route:literal, $stream_request:ty, $stream_command:literal),)+ }
        ) => {{
            let mut service = tonic_build::manual::Service::builder()
                .name("MachineRpc")
                .package($package)
                .comment("Machine control RPCs with schema-blind protobuf envelopes.");
            $(service = service.method(method(stringify!($unary_name), $unary_route));)+
            $(service = service.method(streaming_method(stringify!($stream_name), $stream_route));)+
            service
        }};
    }
    let exec = tonic_build::manual::Method::builder()
        .name("exec")
        .route_name("Exec")
        .input_type("crate::rpc::OpaquePayload")
        .output_type("crate::rpc::OpaquePayload")
        .codec_path("tonic::codec::ProstCodec")
        .client_streaming()
        .server_streaming()
        .build();
    let machine_rpc = rpc_catalog!(build_service).method(exec).build();
    tonic_build::manual::Builder::new().compile(&[machine_rpc]);
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
