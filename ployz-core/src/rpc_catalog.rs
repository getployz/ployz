/// The RPC catalog: one row per Machine RPC, read by every consumer that must stay
/// in step with it — the tonic service (build.rs), the request enum, typed `call`,
/// request extractors on `op` and `Rpc`, and capability advertisement.
///
/// Columns, in order:
///
/// ```text
/// Variant: (method, route, request, command, response, CAPABILITY, "capability.v1", Advertisement)
/// ```
///
/// Streaming rows have no single response and name none:
///
/// ```text
/// Variant: (method, route, request, command, CAPABILITY, "capability.v1", Advertisement)
/// ```
///
/// `request` is the caller-facing payload (`Rpc::Request`). `response` is the
/// envelope that RPC resolves to (`Rpc::Response`). Several requests may share
/// one envelope.
///
/// `CAPABILITY` is the stable advertised name for that RPC. `Advertisement` is
/// `Always`, `Container`, `Caddy`, or `Cluster` — the group `describe_contract`
/// includes the name in.
///
/// `package` is the gRPC service package. It is stated here rather than at each consumer
/// so a service-version bump is one edit (plus the `include!` path in `rpc.rs`, which the
/// compiler requires to be a literal).
///
/// `Exec` is deliberately absent: it is bidirectionally streamed and schema-blind, so it
/// has no request variant and is wired up by hand. Its capability is an explicit extra
/// Container row on the advertisement table.
#[macro_export]
macro_rules! rpc_catalog {
    ($consumer:ident) => {
        $consumer! {
            package "ployz.rpc.v1"
            unary {
                DescribeContract: (describe_contract, "DescribeContract", DescribeContractRequest, "describe_contract", ContractDescription, DESCRIBE_CONTRACT_CAPABILITY, "ployz.rpc.describe-contract.v1", Always),
                Inspect: (inspect, "Inspect", InspectRequest, "inspect", MachineDetails, INSPECT_MACHINE_CAPABILITY, "ployz.machine.inspect.v1", Always),
                MachineToken: (machine_token, "MachineToken", MachineTokenRequest, "machine_token", MachineToken, MACHINE_TOKEN_CAPABILITY, "ployz.machine.token.v1", Always),
                Initialize: (initialize, "Initialize", InitializeRequest, "initialize", Initialized, INITIALIZE_MACHINE_CAPABILITY, "ployz.machine.initialize.v1", Always),
                Register: (register, "Register", RegisterRequest, "register", Registered, REGISTER_MACHINE_CAPABILITY, "ployz.machine.register.v1", Always),
                Join: (join, "Join", JoinRequest, "join", JoinAccepted, JOIN_MACHINE_CAPABILITY, "ployz.machine.join.v1", Always),
                SetCloudPairing: (set_cloud_pairing, "SetCloudPairing", SetCloudPairingRequest, "set_cloud_pairing", CloudPairingSet, SET_CLOUD_PAIRING_CAPABILITY, "ployz.machine.set-cloud-pairing.v1", Always),
                ListMachines: (list_machines, "ListMachines", ListMachinesRequest, "list_machines", MachineList, LIST_MACHINES_CAPABILITY, "ployz.machine.list.v1", Always),
                UpdateMachine: (update_machine, "UpdateMachine", UpdateMachineRequest, "update_machine", MachineUpdated, UPDATE_MACHINE_CAPABILITY, "ployz.machine.update.v1", Always),
                RemoveLocalMachine: (remove_local_machine, "RemoveLocalMachine", RemoveLocalMachineRequest, "remove_local_machine", LocalMachineRemoved, REMOVE_LOCAL_MACHINE_CAPABILITY, "ployz.machine.remove-local.v1", Always),
                RemoveMachine: (remove_machine, "RemoveMachine", RemoveMachineRequest, "remove_machine", MachineRemoved, REMOVE_MACHINE_CAPABILITY, "ployz.machine.remove.v1", Always),
                InspectWireguard: (inspect_wireguard, "InspectWireguard", InspectWireGuardRequest, "inspect_wireguard", WireGuardInspected, INSPECT_WIREGUARD_CAPABILITY, "ployz.wireguard.inspect.v1", Always),
                ListContainers: (list_containers, "ListContainers", ListContainersRequest, "list_containers", ContainerList, LIST_CONTAINERS_CAPABILITY, "ployz.container.list.v1", Container),
                InspectContainer: (inspect_container, "InspectContainer", InspectContainerRequest, "inspect_container", ContainerDetails, INSPECT_CONTAINER_CAPABILITY, "ployz.container.inspect.v1", Container),
                CreateContainer: (create_container, "CreateContainer", CreateContainerRequest, "create_container", ContainerCreated, CREATE_CONTAINER_CAPABILITY, "ployz.container.create.v1", Container),
                EnsureGlobalSlot: (ensure_global_slot, "EnsureGlobalSlot", EnsureGlobalSlotRequest, "ensure_global_slot", ContainerCreated, ENSURE_GLOBAL_SLOT_CAPABILITY, "ployz.container.ensure-global-slot.v1", Container),
                StartContainer: (start_container, "StartContainer", StartContainerRequest, "start_container", ContainerChanged, START_CONTAINER_CAPABILITY, "ployz.container.start.v1", Container),
                StopContainer: (stop_container, "StopContainer", StopContainerRequest, "stop_container", ContainerChanged, STOP_CONTAINER_CAPABILITY, "ployz.container.stop.v1", Container),
                RemoveContainer: (remove_container, "RemoveContainer", RemoveContainerRequest, "remove_container", ContainerChanged, REMOVE_CONTAINER_CAPABILITY, "ployz.container.remove.v1", Container),
                CreateVolume: (create_volume, "CreateVolume", CreateVolumeRequest, "create_volume", DockerVolume, CREATE_VOLUME_CAPABILITY, "ployz.volume.create.v1", Container),
                ListVolumes: (list_volumes, "ListVolumes", ListVolumesRequest, "list_volumes", VolumeList, LIST_VOLUMES_CAPABILITY, "ployz.volume.list.v1", Container),
                InspectVolume: (inspect_volume, "InspectVolume", InspectVolumeRequest, "inspect_volume", DockerVolume, INSPECT_VOLUME_CAPABILITY, "ployz.volume.inspect.v1", Container),
                RemoveVolume: (remove_volume, "RemoveVolume", RemoveVolumeRequest, "remove_volume", VolumeRemoved, REMOVE_VOLUME_CAPABILITY, "ployz.volume.remove.v1", Container),
                ListImages: (list_images, "ListImages", ListImagesRequest, "list_images", MachineImages, LIST_IMAGES_CAPABILITY, "ployz.image.list.v1", Container),
                EnsureImageIngest: (ensure_image_ingest, "EnsureImageIngest", EnsureImageIngestRequest, "ensure_image_ingest", ImageIngestOpened, ENSURE_IMAGE_INGEST_CAPABILITY, "ployz.image.ingest.ensure.v1", Container),
                PullImageFromMachine: (pull_image_from_machine, "PullImageFromMachine", PullImageFromMachineRequest, "pull_image_from_machine", ImagePulled, PULL_IMAGE_FROM_MACHINE_CAPABILITY, "ployz.image.pull-from-machine.v1", Container),
                GetCaddyConfig: (get_caddy_config, "GetCaddyConfig", GetCaddyConfigRequest, "get_caddy_config", CaddyConfig, GET_CADDY_CONFIG_CAPABILITY, "ployz.caddy.config.v1", Caddy),
                ReserveDomain: (reserve_domain, "ReserveDomain", ReserveDomainRequest, "reserve_domain", Domain, RESERVE_DOMAIN_CAPABILITY, "ployz.dns.reserve.v1", Cluster),
                GetDomain: (get_domain, "GetDomain", GetDomainRequest, "get_domain", Domain, GET_DOMAIN_CAPABILITY, "ployz.dns.show.v1", Cluster),
                ReleaseDomain: (release_domain, "ReleaseDomain", ReleaseDomainRequest, "release_domain", Domain, RELEASE_DOMAIN_CAPABILITY, "ployz.dns.release.v1", Cluster),
                CreateDomainRecords: (create_domain_records, "CreateDomainRecords", CreateDomainRecordsRequest, "create_domain_records", DomainRecords, CREATE_DOMAIN_RECORDS_CAPABILITY, "ployz.dns.records.create.v1", Cluster),
                Reset: (reset, "Reset", ResetRequest, "reset", ResetAccepted, RESET_MACHINE_CAPABILITY, "ployz.machine.reset.v1", Always),
            }
            server_streaming {
                ContainerLogs: (container_logs, "ContainerLogs", ContainerLogsRequest, "container_logs", CONTAINER_LOGS_CAPABILITY, "ployz.container.logs.v1", Container),
                MachineLogs: (machine_logs, "MachineLogs", MachineLogsRequest, "machine_logs", MACHINE_LOGS_CAPABILITY, "ployz.machine.logs.v1", Container),
                RuntimeWatch: (runtime_watch, "RuntimeWatch", RuntimeWatchRequest, "runtime_watch", RUNTIME_WATCH_CAPABILITY, "ployz.runtime.watch.v1", Always),
            }
        }
    };
}
