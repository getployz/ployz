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
    let service = tonic_build::manual::Service::builder()
        .name("CloudRelay")
        .package("ployz.relay.v1")
        .comment("Cloud Relay Register, Dial, and Attach. Inner bytes are opaque.")
        .method(register)
        .method(dial)
        .method(attach)
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
