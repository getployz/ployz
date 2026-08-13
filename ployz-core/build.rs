fn main() {
    let describe_contract = tonic_build::manual::Method::builder()
        .name("describe_contract")
        .route_name("DescribeContract")
        .input_type("crate::rpc::OpaquePayload")
        .output_type("crate::rpc::OpaquePayload")
        .codec_path("tonic::codec::ProstCodec")
        .build();
    let reset = tonic_build::manual::Method::builder()
        .name("reset")
        .route_name("Reset")
        .input_type("crate::rpc::OpaquePayload")
        .output_type("crate::rpc::OpaquePayload")
        .codec_path("tonic::codec::ProstCodec")
        .build();
    let machine_rpc = tonic_build::manual::Service::builder()
        .name("MachineRpc")
        .package("ployz.rpc.v1")
        .comment("Machine control RPCs with schema-blind protobuf envelopes.")
        .method(describe_contract)
        .method(reset)
        .build();
    tonic_build::manual::Builder::new().compile(&[machine_rpc]);
}
