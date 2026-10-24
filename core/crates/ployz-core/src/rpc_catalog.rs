/// The RPC catalog: one row per Machine RPC, read by every consumer that must stay
/// in step with it — the tonic service (build.rs), the request enum, typed `call`,
/// request extractors on `op` and `Rpc`, and capability advertisement.
///
/// Columns, in order:
///
/// ```text
/// Variant: (method, route, request, command, response, CAPABILITY, "capability.v1", Advertisement, Scope)
/// ```
///
/// Streaming rows have no single response and name none:
///
/// ```text
/// Variant: (method, route, request, command, CAPABILITY, "capability.v1", Advertisement, Scope)
/// ```
///
/// `request` is the caller-facing payload (`Rpc::Request`). `response` is the
/// envelope that RPC resolves to (`Rpc::Response`). Several requests may share
/// one envelope.
///
/// `CAPABILITY` is the stable advertised name for that RPC. `Advertisement` is
/// `Always`, `Container`, or `Cluster` — the group `describe_contract`
/// includes the name in. `Scope` is the least [`ManagementScope`](crate::ManagementScope)
/// a Management Client needs to call it: `Operator` or `Full`.
///
/// `package` is the gRPC service package. It is stated here rather than at each consumer
/// so a service-version bump is one edit (plus the `include!` path in `rpc.rs`, which the
/// compiler requires to be a literal).
///
/// Bidirectional rows are schema-blind streams of `OpaquePayload`, so they name no
/// request either:
///
/// ```text
/// Variant: (method, route, command, CAPABILITY, "capability.v1", Advertisement, Scope)
/// ```
///
/// `capabilities` rows name a capability with no route of its own: a behaviour of a
/// route above that a caller may need to know a Machine has.
///
/// ```text
/// CAPABILITY: ("capability.v1", Advertisement)
/// ```
///
/// Removing a row removes the RPC with no shim: the daemon stops advertising its
/// capability and refuses the route, and a caller names the capability it lacks.
#[macro_export]
macro_rules! rpc_catalog {
    ($consumer:ident) => {
        $consumer! {
            package "ployz.rpc.v1"
            unary {
                DescribeContract: (describe_contract, "DescribeContract", DescribeContractRequest, "describe_contract", ContractDescription, DESCRIBE_CONTRACT_CAPABILITY, "ployz.rpc.describe-contract.v1", Always, Operator),
                Inspect: (inspect, "Inspect", InspectRequest, "inspect", MachineDetails, INSPECT_MACHINE_CAPABILITY, "ployz.machine.inspect.v1", Always, Operator),
                Enroll: (enroll, "Enroll", EnrollRequest, "enroll", Enrolled, ENROLL_MACHINE_CAPABILITY, "ployz.machine.enroll.v1", Always, Operator),
                EnrollForRecovery: (enroll_for_recovery, "EnrollForRecovery", EnrollForRecoveryRequest, "enroll_for_recovery", Enrolled, ENROLL_FOR_RECOVERY_CAPABILITY, "ployz.machine.enroll-for-recovery.v1", Always, Operator),
                WrapSealingKey: (wrap_sealing_key, "WrapSealingKey", WrapSealingKeyRequest, "wrap_sealing_key", SealingKeyWrapped, WRAP_SEALING_KEY_CAPABILITY, "ployz.sealing.wrap.v1", Always, Full),
                RecoverSealingKey: (recover_sealing_key, "RecoverSealingKey", RecoverSealingKeyRequest, "recover_sealing_key", SealingKeyRecovered, RECOVER_SEALING_KEY_CAPABILITY, "ployz.sealing.recover.v1", Always, Full),
                Register: (register, "Register", RegisterRequest, "register", Registered, REGISTER_MACHINE_CAPABILITY, "ployz.machine.register.v1", Always, Operator),
                SetManagementClient: (set_management_client, "SetManagementClient", SetManagementClientRequest, "set_management_client", SetManagementClientResponse, SET_MANAGEMENT_CLIENT_CAPABILITY, "ployz.machine.set-management-client.v1", Always, Operator),
                SetDevice: (set_device, "SetDevice", SetDeviceRequest, "set_device", DeviceSet, SET_DEVICE_CAPABILITY, "ployz.machine.set-device.v1", Always, Full),
                SetPushTarget: (set_push_target, "SetPushTarget", SetPushTargetRequest, "set_push_target", PushTargetSet, SET_PUSH_TARGET_CAPABILITY, "ployz.machine.set-push-target.v1", Always, Operator),
                SelectEndpoint: (select_endpoint, "SelectEndpoint", SelectEndpointRequest, "select_endpoint", EndpointSelected, SELECT_ENDPOINT_CAPABILITY, "ployz.network.select-endpoint.v1", Always, Operator),
                ListMachines: (list_machines, "ListMachines", ListMachinesRequest, "list_machines", MachineList, LIST_MACHINES_CAPABILITY, "ployz.machine.list.v1", Always, Operator),
                UpdateMachine: (update_machine, "UpdateMachine", UpdateMachineRequest, "update_machine", MachineUpdated, UPDATE_MACHINE_CAPABILITY, "ployz.machine.update.v1", Always, Operator),
                RequestMachineUpgrade: (request_machine_upgrade, "RequestMachineUpgrade", RequestMachineUpgradeRequest, "request_machine_upgrade", MachineUpgradeAttempt, REQUEST_MACHINE_UPGRADE_CAPABILITY, "ployz.machine.upgrade.request.v1", Always, Operator),
                GenerateSealingKey: (generate_sealing_key, "GenerateSealingKey", GenerateSealingKeyRequest, "generate_sealing_key", SealingKeyHeld, GENERATE_SEALING_KEY_CAPABILITY, "ployz.sealing.generate.v1", Always, Operator),
                ShareSealingKey: (share_sealing_key, "ShareSealingKey", ShareSealingKeyRequest, "share_sealing_key", SealingKeyShared, SHARE_SEALING_KEY_CAPABILITY, "ployz.sealing.share.v1", Always, Operator),
                InstallSealingKey: (install_sealing_key, "InstallSealingKey", InstallSealingKeyRequest, "install_sealing_key", SealingKeyHeld, INSTALL_SEALING_KEY_CAPABILITY, "ployz.sealing.install.v1", Always, Operator),
                Leave: (leave, "Leave", LeaveRequest, "leave", Left, LEAVE_MACHINE_CAPABILITY, "ployz.machine.leave.v1", Always, Operator),
                Forget: (forget, "Forget", ForgetRequest, "forget", Forgotten, FORGET_MACHINE_CAPABILITY, "ployz.machine.forget.v1", Always, Operator),
                ListContainers: (list_containers, "ListContainers", ListContainersRequest, "list_containers", ContainerList, LIST_CONTAINERS_CAPABILITY, "ployz.container.list.v1", Container, Operator),
                InspectContainer: (inspect_container, "InspectContainer", InspectContainerRequest, "inspect_container", ContainerDetails, INSPECT_CONTAINER_CAPABILITY, "ployz.container.inspect.v1", Container, Operator),
                EnsureContainer: (ensure_container, "EnsureContainer", EnsureContainerRequest, "ensure_container", EnsuredContainer, ENSURE_CONTAINER_CAPABILITY, "ployz.container.ensure.v1", Container, Operator),
                UpdateContainerLimits: (update_container_limits, "UpdateContainerLimits", UpdateContainerLimitsRequest, "update_container_limits", ContainerLimitsUpdated, UPDATE_CONTAINER_LIMITS_CAPABILITY, "ployz.container.limits.update.v1", Container, Operator),
                InspectStorage: (inspect_storage, "InspectStorage", InspectStorageRequest, "inspect_storage", StorageCapacity, INSPECT_STORAGE_CAPABILITY, "ployz.storage.inspect.v1", Container, Operator),
                PrepareVolumes: (prepare_volumes, "PrepareVolumes", PrepareVolumesRequest, "prepare_volumes", PreparedVolumes, PREPARE_VOLUMES_CAPABILITY, "ployz.storage.prepare.v1", Container, Operator),
                ListVolumes: (list_volumes, "ListVolumes", ListVolumesRequest, "list_volumes", VolumeInventory, LIST_VOLUMES_CAPABILITY, "ployz.volume.list.v1", Container, Operator),
                RemoveVolume: (remove_volume, "RemoveVolume", RemoveVolumeRequest, "remove_volume", VolumeRemoved, REMOVE_VOLUME_CAPABILITY, "ployz.volume.remove.v1", Container, Operator),
                InspectVolumeCopy: (inspect_volume_copy, "InspectVolumeCopy", InspectVolumeCopyRequest, "inspect_volume_copy", VolumeCopyView, INSPECT_VOLUME_COPY_CAPABILITY, "ployz.volume.copy.inspect.v1", Container, Operator),
                SeedVolume: (seed_volume, "SeedVolume", SeedVolumeRequest, "seed_volume", VolumeCopyView, SEED_VOLUME_CAPABILITY, "ployz.volume.seed.v1", Container, Operator),
                SetVolumeCopy: (set_volume_copy, "SetVolumeCopy", SetVolumeCopyRequest, "set_volume_copy", SetVolumeCopyReply, SET_VOLUME_COPY_CAPABILITY, "ployz.volume.copy.set.v1", Container, Operator),
                SetVolumeSize: (set_volume_size, "SetVolumeSize", SetVolumeSizeRequest, "set_volume_size", SetVolumeSizeReply, SET_VOLUME_SIZE_CAPABILITY, "ployz.volume.size.set.v1", Container, Operator),
                ListImages: (list_images, "ListImages", ListImagesRequest, "list_images", MachineImages, LIST_IMAGES_CAPABILITY, "ployz.image.list.v1", Container, Operator),
                PruneImages: (prune_images, "PruneImages", PruneImagesRequest, "prune_images", ImagesRemoved, PRUNE_IMAGES_CAPABILITY, "ployz.image.prune.v1", Container, Operator),
                EnsureImageIngest: (ensure_image_ingest, "EnsureImageIngest", EnsureImageIngestRequest, "ensure_image_ingest", ImageIngestOpened, ENSURE_IMAGE_INGEST_CAPABILITY, "ployz.image.ingest.ensure.v1", Container, Operator),
                PullImage: (pull_image, "PullImage", PullImageRequest, "pull_image", ImagePulled, PULL_IMAGE_CAPABILITY, "ployz.image.pull.v1", Container, Operator),
                PullImageFromMachine: (pull_image_from_machine, "PullImageFromMachine", PullImageFromMachineRequest, "pull_image_from_machine", ImagePulled, PULL_IMAGE_FROM_MACHINE_CAPABILITY, "ployz.image.pull-from-machine.v1", Container, Operator),
                MintBuildGrant: (mint_build_grant, "MintBuildGrant", MintBuildGrantRequest, "mint_build_grant", BuildGrantMinted, MINT_BUILD_GRANT_CAPABILITY, "ployz.build.grant.mint.v1", Container, Operator),
                InspectBuildGrant: (inspect_build_grant, "InspectBuildGrant", InspectBuildGrantRequest, "inspect_build_grant", BuildGrantInspected, INSPECT_BUILD_GRANT_CAPABILITY, "ployz.build.grant.inspect.v1", Container, Operator),
                PublishCertificateMaterial: (publish_certificate_material, "PublishCertificateMaterial", PublishCertificateMaterialRequest, "publish_certificate_material", CertificateMaterialPublished, PUBLISH_CERTIFICATE_MATERIAL_CAPABILITY, "ployz.certificates.publish.v1", Cluster, Operator),
                ReadRoutes: (read_routes, "ReadRoutes", ReadRoutesRequest, "read_routes", RoutesRead, READ_ROUTES_CAPABILITY, "ployz.serving.routes.v1", Cluster, Operator),
                ReadRoutedAddresses: (read_routed_addresses, "ReadRoutedAddresses", ReadRoutedAddressesRequest, "read_routed_addresses", RoutedAddresses, READ_ROUTED_ADDRESSES_CAPABILITY, "ployz.serving.routed-addresses.v1", Cluster, Operator),
                MetricHistory: (metric_history, "MetricHistory", MetricHistoryRequest, "metric_history", MetricHistoryReply, METRIC_HISTORY_CAPABILITY, "ployz.metrics.history.v1", Container, Operator),
                RetainedEnv: (retained_env, "RetainedEnv", RetainedEnvRequest, "retained_env", RetainedEnvReply, RETAINED_ENV_CAPABILITY, "ployz.logs.retained-env.v1", Container, Operator),
            }
            server_streaming {
                LogHistory: (log_history, "LogHistory", LogHistoryRequest, "log_history", LOG_HISTORY_CAPABILITY, "ployz.logs.history.v2", Container, Operator),
                TailLogs: (tail_logs, "TailLogs", TailLogsRequest, "tail_logs", TAIL_LOGS_CAPABILITY, "ployz.logs.tail.v1", Container, Operator),
                RuntimeWatch: (runtime_watch, "RuntimeWatch", RuntimeWatchRequest, "runtime_watch", RUNTIME_WATCH_CAPABILITY, "ployz.runtime.watch.v1", Cluster, Operator),
            }
            bidi_streaming {
                Exec: (exec, "Exec", "exec", EXEC_CONTAINER_CAPABILITY, "ployz.container.exec.v1", Container, Full),
                Build: (build, "Build", "build", BUILD_CAPABILITY, "ployz.build.v1", Container, Operator),
            }
            capabilities {
                /// `SetVolumeCopy` honours `gen`, takes the `floor` want and refuses `stale`.
                VOLUME_FLOOR_CAPABILITY: ("ployz.volume.copy.floor.v1", Container),
                /// `SetVolumeSize` takes `allow_shrink` and lowers a bound down to its floor.
                VOLUME_SHRINK_CAPABILITY: ("ployz.volume.size.shrink.v1", Container),
                /// The daemon can take a Certificate Policy from cluster state.
                CERTIFICATE_POLICY_CAPABILITY: ("ployz.certificates.policy.v1", Always),
                /// `Inspect` can report requested current local Machine storage evidence.
                MACHINE_STORAGE_OBSERVATION_CAPABILITY: ("ployz.machine.storage-observation.v1", Always),
                /// The daemon refuses each RPC its caller's Management Scope does not reach.
                MANAGEMENT_SCOPE_CAPABILITY: ("ployz.management.scope.v1", Always),
                /// The daemon opens Sealed values and keeps them out of its own logs.
                ENV_SEALED_CAPABILITY: ("ployz.env.sealed.v1", Always),
                /// The daemon opens a Build's Sealed values as secret files and keeps them out of its output.
                BUILD_SEALED_CAPABILITY: ("ployz.build.sealed.v1", Always),
                /// The daemon mints a Build Grant for a holder's own key and returns no grant secret.
                BUILD_GRANT_HOLDER_CAPABILITY: ("ployz.build.grant.holder.v1", Always),
            }
        }
    };
}
