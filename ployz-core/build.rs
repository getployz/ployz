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
    let inspect = method("inspect", "Inspect");
    let initialize = method("initialize", "Initialize");
    let register = method("register", "Register");
    let join = method("join", "Join");
    let list_machines = method("list_machines", "ListMachines");
    let machine_rpc = tonic_build::manual::Service::builder()
        .name("MachineRpc")
        .package("ployz.rpc.v1")
        .comment("Machine control RPCs with schema-blind protobuf envelopes.")
        .method(describe_contract)
        .method(inspect)
        .method(initialize)
        .method(register)
        .method(join)
        .method(list_machines)
        .method(reset)
        .build();
    tonic_build::manual::Builder::new().compile(&[machine_rpc]);
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
