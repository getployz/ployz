/// The RPC catalog: one row per Machine RPC, read by every consumer that must stay
/// in step with it — the tonic service (build.rs), the request enum, the client
/// dispatch and typed `call`, and the daemon's request extractors.
///
/// Columns, in order:
///
/// ```text
/// Variant: (method, route, request, command, response)
/// ```
///
/// `response` is the response the RPC resolves to, so the request/response pairing is
/// checked by the compiler rather than restated at each call site; several requests may
/// share one response. Streaming rows have no single response and name none.
///
/// `package` is the gRPC service package. It is stated here rather than at each consumer
/// so a service-version bump is one edit (plus the `include!` path in `rpc.rs`, which the
/// compiler requires to be a literal).
///
/// `Exec` is deliberately absent: it is bidirectionally streamed and schema-blind, so it
/// has no request variant and is wired up by hand.
#[macro_export]
macro_rules! rpc_catalog {
    ($consumer:ident) => {
        $consumer! {
            package "ployz.rpc.v1"
            unary {
                DescribeContract: (describe_contract, "DescribeContract", DescribeContractRequest, "describe_contract", ContractDescription),
                Inspect: (inspect, "Inspect", InspectRequest, "inspect", Box<MachineDetails>),
                MachineToken: (machine_token, "MachineToken", MachineTokenRequest, "machine_token", MachineToken),
                Initialize: (initialize, "Initialize", InitializeRequest, "initialize", Initialized),
                Register: (register, "Register", RegisterRequest, "register", Registered),
                Join: (join, "Join", JoinRequest, "join", JoinAccepted),
                ListMachines: (list_machines, "ListMachines", ListMachinesRequest, "list_machines", MachineList),
                UpdateMachine: (update_machine, "UpdateMachine", UpdateMachineRequest, "update_machine", MachineUpdated),
                RemoveLocalMachine: (remove_local_machine, "RemoveLocalMachine", RemoveLocalMachineRequest, "remove_local_machine", LocalMachineRemoved),
                RemoveMachine: (remove_machine, "RemoveMachine", RemoveMachineRequest, "remove_machine", MachineRemoved),
                InspectWireguard: (inspect_wireguard, "InspectWireguard", InspectWireGuardRequest, "inspect_wireguard", WireGuardInspected),
                ListContainers: (list_containers, "ListContainers", ListContainersRequest, "list_containers", ContainerList),
                InspectContainer: (inspect_container, "InspectContainer", InspectContainerRequest, "inspect_container", Box<ContainerDetails>),
                CreateContainer: (create_container, "CreateContainer", Box<CreateContainerRequest>, "create_container", ContainerCreated),
                StartContainer: (start_container, "StartContainer", StartContainerRequest, "start_container", ContainerChanged),
                StopContainer: (stop_container, "StopContainer", StopContainerRequest, "stop_container", ContainerChanged),
                RemoveContainer: (remove_container, "RemoveContainer", RemoveContainerRequest, "remove_container", ContainerChanged),
                CreateVolume: (create_volume, "CreateVolume", CreateVolumeRequest, "create_volume", VolumeCreated),
                ListVolumes: (list_volumes, "ListVolumes", ListVolumesRequest, "list_volumes", VolumeList),
                InspectVolume: (inspect_volume, "InspectVolume", InspectVolumeRequest, "inspect_volume", VolumeDetails),
                RemoveVolume: (remove_volume, "RemoveVolume", RemoveVolumeRequest, "remove_volume", VolumeRemoved),
                ListImages: (list_images, "ListImages", ListImagesRequest, "list_images", MachineImages),
                GetCaddyConfig: (get_caddy_config, "GetCaddyConfig", GetCaddyConfigRequest, "get_caddy_config", CaddyConfig),
                ReserveDomain: (reserve_domain, "ReserveDomain", ReserveDomainRequest, "reserve_domain", Domain),
                GetDomain: (get_domain, "GetDomain", GetDomainRequest, "get_domain", Domain),
                ReleaseDomain: (release_domain, "ReleaseDomain", ReleaseDomainRequest, "release_domain", Domain),
                CreateDomainRecords: (create_domain_records, "CreateDomainRecords", CreateDomainRecordsRequest, "create_domain_records", DomainRecords),
                Reset: (reset, "Reset", ResetRequest, "reset", ResetAccepted),
            }
            server_streaming {
                ContainerLogs: (container_logs, "ContainerLogs", ContainerLogsRequest, "container_logs"),
                MachineLogs: (machine_logs, "MachineLogs", MachineLogsRequest, "machine_logs"),
            }
        }
    };
}
