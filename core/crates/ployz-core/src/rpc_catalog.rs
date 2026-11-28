/// The RPC catalog: one row per Machine RPC, read by every consumer that must stay
/// in step with it — the tonic service (build.rs), the request enum, typed `call`,
/// request extractors on `op` and `Rpc`, and capability advertisement.
///
/// A row names its RPC once, by its variant. The gRPC route is the variant, and the
/// JSON `command` is the method, which the request enum's snake-case tag is. Columns,
/// in order:
///
/// ```text
/// Variant: (method, request, response, CAPABILITY, "capability.v1", Advertisement, Scope)
/// ```
///
/// Streaming rows have no single response and name none:
///
/// ```text
/// Variant: (method, request, CAPABILITY, "capability.v1", Advertisement, Scope)
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
/// Variant: (method, CAPABILITY, "capability.v1", Advertisement, Scope)
/// ```
///
/// `capabilities` rows name a capability with no route of its own: a behaviour of a
/// route above that a caller may need to know a Machine has.
///
/// ```text
/// CAPABILITY: ("capability.v1", Advertisement)
/// ```
///
/// A new column is appended to the end of a row. A consumer that needs types or
/// idents (the request enum, the tonic build, the fixtures) ends its row pattern with
/// `$(, $($more:tt)*)?` and so ignores the columns it does not read; the one consumer
/// that reads the new column names it. `crate::rpc::ROUTES` is the table of every
/// name a consumer reads as data (path, command, capability, advertisement, scope),
/// so a reader that needs no type reads that and never matches a row.
///
/// Removing a row removes the RPC with no shim: the daemon stops advertising its
/// capability and refuses the route, and a caller names the capability it lacks.
#[macro_export]
macro_rules! rpc_catalog {
    ($consumer:ident) => {
        $consumer! {
            package "ployz.rpc.v1"
            unary {
                DescribeContract: (describe_contract, DescribeContractRequest, ContractDescription, DESCRIBE_CONTRACT_CAPABILITY, "ployz.rpc.describe-contract.v1", Always, Operator),
                Inspect: (inspect, InspectRequest, MachineDetails, INSPECT_MACHINE_CAPABILITY, "ployz.machine.inspect.v1", Always, Operator),
                Enroll: (enroll, EnrollRequest, Enrolled, ENROLL_MACHINE_CAPABILITY, "ployz.machine.enroll.v1", Always, Operator),
                EnrollForRecovery: (enroll_for_recovery, EnrollForRecoveryRequest, Enrolled, ENROLL_FOR_RECOVERY_CAPABILITY, "ployz.machine.enroll-for-recovery.v1", Always, Operator),
                WrapSealingKey: (wrap_sealing_key, WrapSealingKeyRequest, SealingKeyWrapped, WRAP_SEALING_KEY_CAPABILITY, "ployz.sealing.wrap.v1", Always, Full),
                RecoverSealingKey: (recover_sealing_key, RecoverSealingKeyRequest, SealingKeyRecovered, RECOVER_SEALING_KEY_CAPABILITY, "ployz.sealing.recover.v1", Always, Full),
                Register: (register, RegisterRequest, Registered, REGISTER_MACHINE_CAPABILITY, "ployz.machine.register.v1", Always, Operator),
                SetManagementClient: (set_management_client, SetManagementClientRequest, SetManagementClientResponse, SET_MANAGEMENT_CLIENT_CAPABILITY, "ployz.machine.set-management-client.v1", Always, Operator),
                PendingJoins: (pending_joins, PendingJoinsRequest, PendingJoins, PENDING_JOINS_CAPABILITY, "ployz.machine.join.pending.v1", Always, Operator),
                ApproveJoin: (approve_join, ApproveJoinRequest, JoinApproved, APPROVE_JOIN_CAPABILITY, "ployz.machine.join.approve.v1", Always, Operator),
                SetDevice: (set_device, SetDeviceRequest, DeviceSet, SET_DEVICE_CAPABILITY, "ployz.machine.set-device.v1", Always, Full),
                SetKey: (set_key, SetKeyRequest, DeviceSet, SET_KEY_CAPABILITY, "ployz.machine.set-key.v1", Always, Full),
                ConfirmJoin: (confirm_join, ConfirmJoinRequest, JoinConfirmed, CONFIRM_JOIN_CAPABILITY, "ployz.machine.join.confirm.v1", Always, Operator),
                SetPushTarget: (set_push_target, SetPushTargetRequest, PushTargetSet, SET_PUSH_TARGET_CAPABILITY, "ployz.machine.set-push-target.v1", Always, Operator),
                KeyStep: (key_step, KeyStepRequest, DeviceSet, KEY_STEP_CAPABILITY, "ployz.machine.key-step.v1", Always, Operator),
                ForgetVersions: (forget_versions, ForgetVersionsRequest, VersionsForgotten, FORGET_VERSIONS_CAPABILITY, "ployz.machine.forget-versions.v1", Always, Operator),
                SelectEndpoint: (select_endpoint, SelectEndpointRequest, EndpointSelected, SELECT_ENDPOINT_CAPABILITY, "ployz.network.select-endpoint.v1", Always, Operator),
                ListMachines: (list_machines, ListMachinesRequest, MachineList, LIST_MACHINES_CAPABILITY, "ployz.machine.list.v1", Always, Operator),
                UpdateMachine: (update_machine, UpdateMachineRequest, MachineUpdated, UPDATE_MACHINE_CAPABILITY, "ployz.machine.update.v1", Always, Operator),
                RequestMachineUpgrade: (request_machine_upgrade, RequestMachineUpgradeRequest, MachineUpgradeAttempt, REQUEST_MACHINE_UPGRADE_CAPABILITY, "ployz.machine.upgrade.request.v1", Always, Operator),
                GenerateSealingKey: (generate_sealing_key, GenerateSealingKeyRequest, SealingKeyHeld, GENERATE_SEALING_KEY_CAPABILITY, "ployz.sealing.generate.v1", Always, Operator),
                ShareSealingKey: (share_sealing_key, ShareSealingKeyRequest, SealingKeyShared, SHARE_SEALING_KEY_CAPABILITY, "ployz.sealing.share.v1", Always, Operator),
                InstallSealingKey: (install_sealing_key, InstallSealingKeyRequest, SealingKeyHeld, INSTALL_SEALING_KEY_CAPABILITY, "ployz.sealing.install.v1", Always, Operator),
                Leave: (leave, LeaveRequest, Left, LEAVE_MACHINE_CAPABILITY, "ployz.machine.leave.v1", Always, Operator),
                Forget: (forget, ForgetRequest, Forgotten, FORGET_MACHINE_CAPABILITY, "ployz.machine.forget.v1", Always, Operator),
                ListContainers: (list_containers, ListContainersRequest, ContainerList, LIST_CONTAINERS_CAPABILITY, "ployz.container.list.v1", Container, Operator),
                InspectContainer: (inspect_container, InspectContainerRequest, ContainerDetails, INSPECT_CONTAINER_CAPABILITY, "ployz.container.inspect.v1", Container, Operator),
                EnsureContainer: (ensure_container, EnsureContainerRequest, EnsuredContainer, ENSURE_CONTAINER_CAPABILITY, "ployz.container.ensure.v1", Container, Operator),
                EnsurePinnedContainer: (ensure_pinned_container, EnsureContainerRequest, EnsuredContainer, ENSURE_PINNED_CONTAINER_CAPABILITY, "ployz.container.ensure-pinned.v1", Container, Operator),
                UpdateContainerLimits: (update_container_limits, UpdateContainerLimitsRequest, ContainerLimitsUpdated, UPDATE_CONTAINER_LIMITS_CAPABILITY, "ployz.container.limits.update.v1", Container, Operator),
                UpdateContainerPlacement: (update_container_placement, UpdateContainerPlacementRequest, ContainerPlacementUpdated, UPDATE_CONTAINER_PLACEMENT_CAPABILITY, "ployz.container.placement.update.v1", Container, Operator),
                InspectStorage: (inspect_storage, InspectStorageRequest, StorageCapacity, INSPECT_STORAGE_CAPABILITY, "ployz.storage.inspect.v1", Container, Operator),
                PrepareVolumes: (prepare_volumes, PrepareVolumesRequest, PreparedVolumes, PREPARE_VOLUMES_CAPABILITY, "ployz.storage.prepare.v1", Container, Operator),
                ListVolumes: (list_volumes, ListVolumesRequest, VolumeInventory, LIST_VOLUMES_CAPABILITY, "ployz.volume.list.v1", Container, Operator),
                RemoveVolume: (remove_volume, RemoveVolumeRequest, VolumeRemoved, REMOVE_VOLUME_CAPABILITY, "ployz.volume.remove.v1", Container, Operator),
                ListKeptVolumes: (list_kept_volumes, ListKeptVolumesRequest, KeptVolumes, LIST_KEPT_VOLUMES_CAPABILITY, "ployz.volume.kept.list.v1", Container, Operator),
                RemoveKeptVolumes: (remove_kept_volumes, RemoveKeptVolumesRequest, KeptVolumesRemoved, REMOVE_KEPT_VOLUMES_CAPABILITY, "ployz.volume.kept.remove.v1", Container, Operator),
                InspectVolumeCopy: (inspect_volume_copy, InspectVolumeCopyRequest, VolumeCopyView, INSPECT_VOLUME_COPY_CAPABILITY, "ployz.volume.copy.inspect.v1", Container, Operator),
                SeedVolume: (seed_volume, SeedVolumeRequest, VolumeCopyView, SEED_VOLUME_CAPABILITY, "ployz.volume.seed.v1", Container, Full),
                SeedCopy: (seed_copy, SeedCopyRequest, VolumeCopyView, SEED_COPY_CAPABILITY, "ployz.volume.seed-copy.v1", Container, Operator),
                SetVolumeCopy: (set_volume_copy, SetVolumeCopyRequest, SetVolumeCopyReply, SET_VOLUME_COPY_CAPABILITY, "ployz.volume.copy.set.v1", Container, Operator),
                SetVolumeSize: (set_volume_size, SetVolumeSizeRequest, SetVolumeSizeReply, SET_VOLUME_SIZE_CAPABILITY, "ployz.volume.size.set.v1", Container, Operator),
                ListTrash: (list_trash, ListTrashRequest, TrashList, LIST_TRASH_CAPABILITY, "ployz.volume.trash.v1", Container, Operator),
                RestoreTrash: (restore_trash, RestoreTrashRequest, TrashRestored, RESTORE_TRASH_CAPABILITY, "ployz.volume.trash-restore.v1", Container, Operator),
                EmptyTrash: (empty_trash, EmptyTrashRequest, TrashEmptied, EMPTY_TRASH_CAPABILITY, "ployz.volume.trash-empty.v1", Container, Operator),
                ClosePreview: (close_preview, ClosePreviewRequest, PreviewClosed, CLOSE_PREVIEW_CAPABILITY, "ployz.preview.close.v1", Container, Operator),
                ListPreviews: (list_previews, ListPreviewsRequest, PreviewsHeld, LIST_PREVIEWS_CAPABILITY, "ployz.preview.list.v1", Container, Operator),
                ListImages: (list_images, ListImagesRequest, MachineImages, LIST_IMAGES_CAPABILITY, "ployz.image.list.v1", Container, Operator),
                PruneImages: (prune_images, PruneImagesRequest, ImagesRemoved, PRUNE_IMAGES_CAPABILITY, "ployz.image.prune.v1", Container, Operator),
                EnsureImageIngest: (ensure_image_ingest, EnsureImageIngestRequest, ImageIngestOpened, ENSURE_IMAGE_INGEST_CAPABILITY, "ployz.image.ingest.ensure.v1", Container, Operator),
                PullImage: (pull_image, PullImageRequest, ImagePulled, PULL_IMAGE_CAPABILITY, "ployz.image.pull.v1", Container, Operator),
                PullFreeImage: (pull_free_image, PullFreeImageRequest, ImagePulled, PULL_FREE_IMAGE_CAPABILITY, "ployz.image.pull-free.v1", Container, Operator),
                PullImageFromMachine: (pull_image_from_machine, PullImageFromMachineRequest, ImagePulled, PULL_IMAGE_FROM_MACHINE_CAPABILITY, "ployz.image.pull-from-machine.v1", Container, Operator),
                MintBuildGrant: (mint_build_grant, MintBuildGrantRequest, BuildGrantMinted, MINT_BUILD_GRANT_CAPABILITY, "ployz.build.grant.mint.v1", Container, Operator),
                InspectBuildGrant: (inspect_build_grant, InspectBuildGrantRequest, BuildGrantInspected, INSPECT_BUILD_GRANT_CAPABILITY, "ployz.build.grant.inspect.v1", Container, Operator),
                PublishCertificateMaterial: (publish_certificate_material, PublishCertificateMaterialRequest, CertificateMaterialPublished, PUBLISH_CERTIFICATE_MATERIAL_CAPABILITY, "ployz.certificates.publish.v1", Cluster, Operator),
                ReadRoutes: (read_routes, ReadRoutesRequest, RoutesRead, READ_ROUTES_CAPABILITY, "ployz.serving.routes.v1", Cluster, Operator),
                ReadRoutedAddresses: (read_routed_addresses, ReadRoutedAddressesRequest, RoutedAddresses, READ_ROUTED_ADDRESSES_CAPABILITY, "ployz.serving.routed-addresses.v1", Cluster, Operator),
                SetIngressDoors: (set_ingress_doors, SetIngressDoorsRequest, IngressDoorsSet, SET_INGRESS_DOORS_CAPABILITY, "ployz.ingress.doors.v1", Cluster, Operator),
                SetPrivateAccess: (set_private_access, SetPrivateAccessRequest, PrivateAccessSet, SET_PRIVATE_ACCESS_CAPABILITY, "ployz.private-access.v1", Cluster, Operator),
                ReadClusterName: (read_cluster_name, ReadClusterNameRequest, ClusterNamed, READ_CLUSTER_NAME_CAPABILITY, "ployz.cluster.name.read.v1", Cluster, Operator),
                SetClusterName: (set_cluster_name, SetClusterNameRequest, ClusterNamed, SET_CLUSTER_NAME_CAPABILITY, "ployz.cluster.name.v1", Cluster, Operator),
                SetClusterDoors: (set_cluster_doors, SetClusterDoorsRequest, ClusterDoorsDeclared, SET_CLUSTER_DOORS_CAPABILITY, "ployz.cluster.doors.v1", Cluster, Operator),
                MetricsLive: (metrics_live, MetricsLiveRequest, MetricsLiveReply, METRICS_LIVE_CAPABILITY, "ployz.metrics.live.v1", Container, Operator),
            }
            server_streaming {
                TailLogs: (tail_logs, TailLogsRequest, TAIL_LOGS_CAPABILITY, "ployz.logs.tail.v1", Container, Operator),
                RuntimeWatch: (runtime_watch, RuntimeWatchRequest, RUNTIME_WATCH_CAPABILITY, "ployz.runtime.watch.v1", Cluster, Operator),
            }
            bidi_streaming {
                Exec: (exec, EXEC_CONTAINER_CAPABILITY, "ployz.container.exec.v1", Container, Full),
                Build: (build, BUILD_CAPABILITY, "ployz.build.v1", Container, Operator),
                BuildFree: (build_free, BUILD_FREE_CAPABILITY, "ployz.build.free.v1", Container, Operator),
            }
            capabilities {
                JOIN_APPROVAL_CAPABILITY: ("ployz.machine.join.device-approval.v1", Always),
                /// A joiner that claims it opens the trust an admitting Machine seals in its
                /// capsule, and takes that Machine's restrictions with `ConfirmJoin` before it
                /// participates. An admitting Machine that advertises it seals them.
                JOIN_TRUST_CAPABILITY: ("ployz.machine.join-trust.v1", Always),
                /// `SetVolumeCopy` honours `gen`, takes the `floor` want and refuses `stale`.
                VOLUME_FLOOR_CAPABILITY: ("ployz.volume.copy.floor.v1", Container),
                /// `SetVolumeSize` takes `allow_shrink` and lowers a bound down to its floor.
                VOLUME_SHRINK_CAPABILITY: ("ployz.volume.size.shrink.v1", Container),
                /// The Machine opens a Volume's data only for a Container whose spec expects the origin it carries, and its pulls take born data.
                VOLUME_ORIGIN_CAPABILITY: ("ployz.volume.origin.v1", Container),
                /// The Machine admits a create, start or data arrival in a preview Namespace only when the Namespace names it as home, and a preview Container only when it is signed and mounts only tmpfs and the preview's own Volumes.
                PREVIEW_HOME_CAPABILITY: ("ployz.preview.home.v1", Container),
                /// `SetVolumeCopy` creates an Operator's Writer from the signed versions or rule proof its `Create` carries, for a spec that mounts the Volume, and answers a `check` without changing anything.
                VOLUME_START_CAPABILITY: ("ployz.volume.start.v1", Container),
                /// A Writer start's promote records the image the start placed, with its epoch and the admitted spec's digest, and a start resent after the promote creates its holder from that image with no pull. A start whose volume plugin promotes without that record is refused.
                VOLUME_START_IMAGE_CAPABILITY: ("ployz.volume.start.image.v1", Container),
                /// `SetVolumeCopy`'s cut with a `holder` takes `running`: it refuses `stopped` when that Container no longer runs, and records a stop it makes before making it, so a resent cut knows the stop as its own.
                VOLUME_CUT_RUNNING_CAPABILITY: ("ployz.volume.cut.running.v1", Container),
                /// `SetVolumeCopy` keeps a slot that may hold writes no other Machine has: a Forget without `discard` refuses `written_here`, and a pull never replaces it with a full send.
                VOLUME_KEEP_WRITTEN_CAPABILITY: ("ployz.volume.keep-written.v1", Container),
                /// The daemon can take a Certificate Policy from cluster state.
                CERTIFICATE_POLICY_CAPABILITY: ("ployz.certificates.policy.v1", Always),
                /// `Inspect` can report requested current local Machine storage evidence.
                MACHINE_STORAGE_OBSERVATION_CAPABILITY: ("ployz.machine.storage-observation.v1", Always),
                /// The daemon refuses each RPC its caller's Management Scope does not reach.
                MANAGEMENT_SCOPE_CAPABILITY: ("ployz.management.scope.v1", Always),
                /// `RequestMachineUpgrade` takes an exact version no Release Channel resolves to only from a Full caller.
                UPGRADE_EXACT_CAPABILITY: ("ployz.machine.upgrade.exact.v1", Always),
                /// The daemon opens Sealed values and keeps them out of its own logs.
                ENV_SEALED_CAPABILITY: ("ployz.env.sealed.v1", Always),
                /// The daemon opens a Build's Sealed values as secret files and keeps them out of its output.
                BUILD_SEALED_CAPABILITY: ("ployz.build.sealed.v1", Always),
                /// The daemon mints a Build Grant for a holder's own key and returns no grant secret.
                BUILD_GRANT_HOLDER_CAPABILITY: ("ployz.build.grant.holder.v1", Always),
                BUILD_GRANT_RULE_CAPABILITY: ("ployz.build.grant.rule.v1", Always),
                /// `SetIngressDoors` takes the Tunnel Door with a `sealed_token`, and places the Tunnel Connector.
                SET_INGRESS_TUNNEL_CAPABILITY: ("ployz.ingress.tunnel.v1", Cluster),
                /// `SetPrivateAccess` takes a person's approval of its sealed auth key and turns Private Access on for an Operator only with one.
                PRIVATE_ACCESS_APPROVAL_CAPABILITY: ("ployz.private-access.approval.v1", Cluster),
                /// `ListTrash` serves an Operator, and `RestoreTrash` and `EmptyTrash` take a person's approval of the entries they name on this Machine and serve an Operator only with one.
                TRASH_APPROVAL_CAPABILITY: ("ployz.volume.trash.approval.v1", Container),
                /// `EnsureContainer` takes signed versions and creates an Operator's Container only when one signs its spec's digest.
                SIGNED_VERSION_CAPABILITY: ("ployz.container.signed-version.v1", Always),
                /// A signed version may be an unsigned step of a signed proposal bundle that rides in its envelope.
                SIGNED_VERSION_PROPOSAL_CAPABILITY: ("ployz.container.signed-version.proposal.v1", Always),
                /// `EnsureContainer` takes a Free Build's receipt and runs an unsigned creation's Container from exactly the image it names, once its MAC holds under the Cluster's Sealing Key.
                FREE_RECEIPT_CAPABILITY: ("ployz.container.free-receipt.v1", Always),
                /// `EnsureContainer` takes an automatic deploy rule with its image token and creates an Operator's Container under it only for the head's image built under the rule.
                DEPLOY_RULE_CAPABILITY: ("ployz.container.deploy-rule.v1", Always),
                /// `EnsureContainer` takes a registry automatic deploy rule, and creates an Operator's Container under it only from the image this Machine pulled itself from the rule's repository, by the digest the spec names.
                DEPLOY_REGISTRY_RULE_CAPABILITY: ("ployz.container.deploy-registry-rule.v1", Always),
                /// An automatic deploy rule may be answered by a trusted passkey's own prompt, which rides in its envelope, in place of a Device's signature.
                RULE_PASSKEY_CAPABILITY: ("ployz.rule.passkey.v1", Always),
                /// A rule may be a preview rule: it creates a Container only in a preview Namespace carrying its ID, with the Parent's values, Volumes born for that preview and domains naming it, builds any branch its pushers push, runs the images it pins with no build, and runs a push's hook once per preview.
                PREVIEW_RULE_CAPABILITY: ("ployz.container.preview-rule.v1", Always),
                /// The daemon trusts CI and agent keys a person granted, each only for the Service versions its scope names, and admits a join approval carrying them.
                SCOPED_KEYS_CAPABILITY: ("ployz.authority.scoped-keys.v1", Always),
                /// A signed version may name the parentless version a person or full CLI signed that it edits, and a CI or agent key's version must.
                PARENT_VERSIONS_CAPABILITY: ("ployz.authority.parent-versions.v1", Always),
                /// A Key step's trust may name the grant it replaces and the key's name, and applies only over that grant. `SetDevice` takes `remove_grant`, which removes only a CI or agent key, and only over the grants a rotation read.
                KEY_PRIOR_CAPABILITY: ("ployz.authority.key-prior.v1", Always),
                /// A new Container may name the copy it replaces ([`NewContainer::replaces`](crate::NewContainer::replaces)), which the Machine records and reports on that copy (`replaced_by`). A `Gone` takes a Container's anonymous Volumes only when it names the copy the Machine recorded as its replacement ([`AnonymousVolumes::replaced_by`](crate::AnonymousVolumes::replaced_by)) and that copy runs, routed and not stopping. Whatever the caller asked, it keeps them otherwise.
                CONTAINER_REPLACES_CAPABILITY: ("ployz.container.replaces.v1", Container),
            }
        }
    };
}
