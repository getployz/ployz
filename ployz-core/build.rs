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
    let list_containers = method("list_containers", "ListContainers");
    let inspect_container = method("inspect_container", "InspectContainer");
    let create_container = method("create_container", "CreateContainer");
    let start_container = method("start_container", "StartContainer");
    let stop_container = method("stop_container", "StopContainer");
    let remove_container = method("remove_container", "RemoveContainer");
    let create_volume = method("create_volume", "CreateVolume");
    let list_volumes = method("list_volumes", "ListVolumes");
    let inspect_volume = method("inspect_volume", "InspectVolume");
    let remove_volume = method("remove_volume", "RemoveVolume");
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
        .method(list_containers)
        .method(inspect_container)
        .method(create_container)
        .method(start_container)
        .method(stop_container)
        .method(remove_container)
        .method(create_volume)
        .method(list_volumes)
        .method(inspect_volume)
        .method(remove_volume)
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
