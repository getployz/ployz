fn main() {
    let register = bidi(
        "register",
        "Register",
        "crate::RegisterRequest",
        "crate::Open",
    );
    let dial = bidi("dial", "Dial", "crate::TunnelFrame", "crate::TunnelFrame");
    let attach = bidi(
        "attach",
        "Attach",
        "crate::TunnelFrame",
        "crate::TunnelFrame",
    );
    let revoke = tonic_build::manual::Method::builder()
        .name("revoke")
        .route_name("Revoke")
        .input_type("crate::RevokeRequest")
        .output_type("crate::RevokeResponse")
        .codec_path("tonic::codec::ProstCodec")
        .build();
    let service = tonic_build::manual::Service::builder()
        .name("CloudRelay")
        .package("ployz.relay.v1")
        .comment("Cloud Relay Register, Dial, Attach, and pairing Revoke. Inner bytes are opaque.")
        .method(register)
        .method(dial)
        .method(attach)
        .method(revoke)
        .build();
    tonic_build::manual::Builder::new().compile(&[service]);
}

fn bidi(name: &str, route: &str, input: &str, output: &str) -> tonic_build::manual::Method {
    tonic_build::manual::Method::builder()
        .name(name)
        .route_name(route)
        .input_type(input)
        .output_type(output)
        .codec_path("tonic::codec::ProstCodec")
        .client_streaming()
        .server_streaming()
        .build()
}
