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
/// `Always`, `Container`, `Ingress`, or `Cluster` — the group `describe_contract`
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
                Enroll: (enroll, "Enroll", EnrollRequest, "enroll", Enrolled, ENROLL_MACHINE_CAPABILITY, "ployz.machine.enroll.v1", Always),
                Register: (register, "Register", RegisterRequest, "register", Registered, REGISTER_MACHINE_CAPABILITY, "ployz.machine.register.v1", Always),
                SetManagementClient: (set_management_client, "SetManagementClient", SetManagementClientRequest, "set_management_client", SetManagementClientResponse, SET_MANAGEMENT_CLIENT_CAPABILITY, "ployz.machine.set-management-client.v1", Always),
                ListMachines: (list_machines, "ListMachines", ListMachinesRequest, "list_machines", MachineList, LIST_MACHINES_CAPABILITY, "ployz.machine.list.v1", Always),
                UpdateMachine: (update_machine, "UpdateMachine", UpdateMachineRequest, "update_machine", MachineUpdated, UPDATE_MACHINE_CAPABILITY, "ployz.machine.update.v1", Always),
                RequestMachineUpgrade: (request_machine_upgrade, "RequestMachineUpgrade", RequestMachineUpgradeRequest, "request_machine_upgrade", MachineUpgradeAttempt, REQUEST_MACHINE_UPGRADE_CAPABILITY, "ployz.machine.upgrade.request.v1", Always),
                Leave: (leave, "Leave", LeaveRequest, "leave", Left, LEAVE_MACHINE_CAPABILITY, "ployz.machine.leave.v1", Always),
                Forget: (forget, "Forget", ForgetRequest, "forget", Forgotten, FORGET_MACHINE_CAPABILITY, "ployz.machine.forget.v1", Always),
                ListContainers: (list_containers, "ListContainers", ListContainersRequest, "list_containers", ContainerList, LIST_CONTAINERS_CAPABILITY, "ployz.container.list.v1", Container),
                InspectContainer: (inspect_container, "InspectContainer", InspectContainerRequest, "inspect_container", ContainerDetails, INSPECT_CONTAINER_CAPABILITY, "ployz.container.inspect.v1", Container),
                GetContainerObservations: (get_container_observations, "GetContainerObservations", GetContainerObservationsRequest, "get_container_observations", ContainerObservationMap, GET_CONTAINER_OBSERVATIONS_CAPABILITY, "ployz.container.observations.v1", Cluster),
                CreateContainer: (create_container, "CreateContainer", CreateContainerRequest, "create_container", ContainerCreated, CREATE_CONTAINER_CAPABILITY, "ployz.container.create.v1", Container),
                StartContainer: (start_container, "StartContainer", StartContainerRequest, "start_container", ContainerChanged, START_CONTAINER_CAPABILITY, "ployz.container.start.v1", Container),
                StopContainer: (stop_container, "StopContainer", StopContainerRequest, "stop_container", ContainerChanged, STOP_CONTAINER_CAPABILITY, "ployz.container.stop.v1", Container),
                MarkContainerStopping: (mark_container_stopping, "MarkContainerStopping", MarkContainerStoppingRequest, "mark_container_stopping", ContainerDetails, MARK_CONTAINER_STOPPING_CAPABILITY, "ployz.container.mark-stopping.v1", Container),
                RemoveContainer: (remove_container, "RemoveContainer", RemoveContainerRequest, "remove_container", ContainerChanged, REMOVE_CONTAINER_CAPABILITY, "ployz.container.remove.v1", Container),
                EnsureContainer: (ensure_container, "EnsureContainer", EnsureContainerRequest, "ensure_container", EnsuredContainer, ENSURE_CONTAINER_CAPABILITY, "ployz.container.ensure.v1", Container),
                InspectStorage: (inspect_storage, "InspectStorage", InspectStorageRequest, "inspect_storage", StorageCapacity, INSPECT_STORAGE_CAPABILITY, "ployz.storage.inspect.v1", Container),
                PrepareVolumes: (prepare_volumes, "PrepareVolumes", PrepareVolumesRequest, "prepare_volumes", PreparedVolumes, PREPARE_VOLUMES_CAPABILITY, "ployz.storage.prepare.v1", Container),
                ListVolumes: (list_volumes, "ListVolumes", ListVolumesRequest, "list_volumes", VolumeInventory, LIST_VOLUMES_CAPABILITY, "ployz.volume.list.v1", Container),
                RemoveVolume: (remove_volume, "RemoveVolume", RemoveVolumeRequest, "remove_volume", VolumeRemoved, REMOVE_VOLUME_CAPABILITY, "ployz.volume.remove.v1", Container),
                InspectVolumeCopy: (inspect_volume_copy, "InspectVolumeCopy", InspectVolumeCopyRequest, "inspect_volume_copy", VolumeCopyView, INSPECT_VOLUME_COPY_CAPABILITY, "ployz.volume.copy.inspect.v1", Container),
                SeedVolume: (seed_volume, "SeedVolume", SeedVolumeRequest, "seed_volume", VolumeCopyView, SEED_VOLUME_CAPABILITY, "ployz.volume.seed.v1", Container),
                SetVolumeCopy: (set_volume_copy, "SetVolumeCopy", SetVolumeCopyRequest, "set_volume_copy", SetVolumeCopyReply, SET_VOLUME_COPY_CAPABILITY, "ployz.volume.copy.set.v1", Container),
                ListImages: (list_images, "ListImages", ListImagesRequest, "list_images", MachineImages, LIST_IMAGES_CAPABILITY, "ployz.image.list.v1", Container),
                PruneImages: (prune_images, "PruneImages", PruneImagesRequest, "prune_images", ImagesRemoved, PRUNE_IMAGES_CAPABILITY, "ployz.image.prune.v1", Container),
                EnsureImageIngest: (ensure_image_ingest, "EnsureImageIngest", EnsureImageIngestRequest, "ensure_image_ingest", ImageIngestOpened, ENSURE_IMAGE_INGEST_CAPABILITY, "ployz.image.ingest.ensure.v1", Container),
                PullImageFromMachine: (pull_image_from_machine, "PullImageFromMachine", PullImageFromMachineRequest, "pull_image_from_machine", ImagePulled, PULL_IMAGE_FROM_MACHINE_CAPABILITY, "ployz.image.pull-from-machine.v1", Container),
                MintBuildGrant: (mint_build_grant, "MintBuildGrant", MintBuildGrantRequest, "mint_build_grant", BuildGrantMinted, MINT_BUILD_GRANT_CAPABILITY, "ployz.build.grant.mint.v1", Container),
                InspectBuildGrant: (inspect_build_grant, "InspectBuildGrant", InspectBuildGrantRequest, "inspect_build_grant", BuildGrantInspected, INSPECT_BUILD_GRANT_CAPABILITY, "ployz.build.grant.inspect.v1", Container),
                GetIngressProxyConfig: (get_ingress_proxy_config, "GetIngressProxyConfig", GetIngressProxyConfigRequest, "get_ingress_proxy_config", IngressProxyConfig, GET_INGRESS_PROXY_CONFIG_CAPABILITY, "ployz.ingress.config.v1", Ingress),
                PublishCertificateMaterial: (publish_certificate_material, "PublishCertificateMaterial", PublishCertificateMaterialRequest, "publish_certificate_material", CertificateMaterialPublished, PUBLISH_CERTIFICATE_MATERIAL_CAPABILITY, "ployz.certificates.publish.v1", Cluster),
                MetricHistory: (metric_history, "MetricHistory", MetricHistoryRequest, "metric_history", MetricHistoryReply, METRIC_HISTORY_CAPABILITY, "ployz.metrics.history.v1", Container),
            }
            server_streaming {
                LogHistory: (log_history, "LogHistory", LogHistoryRequest, "log_history", LOG_HISTORY_CAPABILITY, "ployz.logs.history.v2", Container),
                TailLogs: (tail_logs, "TailLogs", TailLogsRequest, "tail_logs", TAIL_LOGS_CAPABILITY, "ployz.logs.tail.v1", Container),
                RuntimeWatch: (runtime_watch, "RuntimeWatch", RuntimeWatchRequest, "runtime_watch", RUNTIME_WATCH_CAPABILITY, "ployz.runtime.watch.v1", Cluster),
            }
        }
    };
}
