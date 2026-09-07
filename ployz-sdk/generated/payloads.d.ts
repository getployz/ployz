// Generated from the Rust wire types by `cargo test -p ployz --test sdk_payloads`.
// Do not edit.

export type AdvertisedEndpoint = string;

export type BindPropagation = "private" | "rprivate" | "shared" | "rshared" | "slave" | "rslave";

export type BindRecursive = "disabled" | "writable" | "readonly";

export type ByteQuantity = number;

export type CapabilityName = string;

export type CertificateAvailability = "available" | "pending" | "failure" | "unknown" | string;

export type CertificateBackoff = { failure_kind: CertificateFailureKind, next_attempt_at: string, failures: number, };

export type CertificateFailureKind = "does_not_resolve" | "resolves_elsewhere" | "authority" | string;

export type CertificateObservation = { hostname: IngressHost, status: CertificateAvailability, last_error: string | null, backoff: CertificateBackoff | null, };

export type ClusterDomainLabel = string;

export type ClusterTeardown = { destroyed_projects: Array<ProjectName>, machines: PartialResult<LocalMachineRemoved, RpcError>, pairing_revoked: boolean, };

export type ConfigMount = { config_name: string, 
/**
 * Omission defaults to `/{config_name}`. Admitted specs retain the canonical target.
 */
target: ContainerPath | null, uid: number | null, gid: number | null, mode: number | null, };

export type ConfigSpec = { name: string, content: Array<number>, };

export type ConfiguredHealthcheck = { test: HealthcheckCommand, interval_millis: number | null, timeout_millis: number | null, start_period_millis: number | null, start_interval_millis: number | null, retries: number | null, };

export type ContainerAddress = string;

export type ContainerHostname = string;

export type ContainerId = string & { readonly __brand: "ContainerId" };

export type ContainerKind = "service_container" | "pre_deploy_hook";

export type ContainerLabels = { [key in string]: string };

export type ContainerObservation = { container_id: ContainerId, 
/**
 * Generated Docker name for display, never identity or selection.
 */
display_name: string, 
/**
 * Docker creation time, used only to select the newest observed Service spec.
 */
created_at_unix_nanos: number, machine_id: MachineId, project_name: ProjectName, kind: ContainerKind, runtime: ContainerRuntimeObservation, 
/**
 * Effective Docker health check, including image-inherited configuration.
 */
effective_healthcheck: HealthcheckSpec | null, 
/**
 * Historical spec used to create this container; not a current Service spec.
 */
resolved_spec: ResolvedServiceSpec, address: ContainerAddress | null, labels: { [key in string]: string }, };

export type ContainerPath = string;

export type ContainerResources = { cpu_nanos: CpuNanos | null, memory_bytes: ByteQuantity | null, memory_reservation_bytes: ByteQuantity | null, shared_memory_bytes: ByteQuantity | null, devices: Array<DeviceMapping>, device_reservations: Array<DeviceReservation>, ulimits: { [key in string]: Ulimit }, };

export type ContainerRuntimeObservation = { "state": "created" } | { "state": "running", health: HealthObservation, } | { "state": "paused" } | { "state": "restarting" } | { "state": "exited", code: number, } | { "state": "removing" } | { "state": "dead" } | { "state": "unrecognized", raw: JsonValue, };

export type ContractDescription = { machine_id: MachineId, protocol_major: number, 
/**
 * Diagnostic only. Callers select behavior using capability names.
 */
daemon_version: string, capabilities: Array<CapabilityName>, };

export type CpuNanos = number;

export type DataLoss = { "kind": "docker_volume", id: DockerVolumeId, };

export type DataLossConfirmation = { confirmed: Array<DataLoss>, };

export type DependencyHealthFailure = { "type": "cancelled" } | { "type": "no_containers" } | { "type": "observation", error: RpcError, } | { "type": "container", container_id: ContainerId, failure: HealthFailure, };

export type DeployEvent = { "type": "progress", completed: number, total: number, rows: Array<OperationRow>, } | { "type": "outcome", outcome: DeployOutcome<ExecutionError>, };

export type DeployIntent = { 
/**
 * Project that will own Containers this Deploy creates.
 */
project_name: ProjectName, 
/**
 * Complete desired Services for this Cluster.
 */
target: Array<RequestedServiceSpec>, 
/**
 * Planner knobs for this Deploy, including the selected Service list.
 */
options: PlanOptions, };

export type DeployOperation = { "type": "wait_healthy", machine_id: MachineId, dependent: QualifiedService, dependency: QualifiedService, } | { "type": "run_container", machine_id: MachineId, spec: ResolvedServiceSpec, skip_health_monitor: boolean, } | { "type": "stop_container", machine_id: MachineId, container_id: ContainerId, purpose: StopContainerPurpose, } | { "type": "remove_container", machine_id: MachineId, container_id: ContainerId, } | { "type": "replace_container" } & ReplacementOperation | { "type": "stop_hook", machine_id: MachineId, container_id: ContainerId, } | { "type": "run_hook", machine_id: MachineId, spec: ResolvedServiceSpec, old_hook_containers: Array<[MachineId, ContainerId]>, } | { "type": "remove_volume", id: DockerVolumeId, };

export type DeployOutcome<E> = { "type": "success", completed: Array<DeployOperation>, } | { "type": "failed", completed: Array<DeployOperation>, failed: FailedOperation<E>, unexecuted: Array<DeployOperation>, };

export type DeployPreview = { 
/**
 * Project this preview describes.
 */
project_name: ProjectName, 
/**
 * Pending rows for the operations this snapshot would execute.
 */
operations: Array<OperationRow>, 
/**
 * Observer-relative warnings for this snapshot, including ingress DNS misses.
 */
warnings: Array<DeployWarning>, 
/**
 * Missing managed Docker Volumes the shown container operations would create on their target
 * Machines during Volume Ensure. These are informational, not executable plan rows.
 */
volumes_to_create: Array<VolumeToCreate>, 
/**
 * Visible Services in the Project that Compose no longer declares.
 */
would_remove: Array<QualifiedService>, 
/**
 * Compose-declared Docker Volumes owned by this Project that this Compose
 * input no longer declares. They are not deleted.
 */
preserved_volumes: Array<PreservedVolume>, 
/**
 * Why pruning will not run. `None` means obsolete Services are removed.
 */
prune_refusal: PruneRefusal | null, };

export type DeployWarning = { "type": "observation_failed", kind: ObservationKind, machine_id: MachineId, message: string, } | { "type": "observation_omitted", kind: ObservationKind, machine_id: MachineId, } | { "type": "storage_observation_unknown", 
/**
 * Machine whose storage capability could not be checked.
 */
machine_id: MachineId, } | { "type": "ingress_hostname", message: string, } | { "type": "observer_relative_hostname_conflict" } | { "type": "skipped_dependency_health", dependent: QualifiedService, dependency: QualifiedService, };

export type DeviceMapping = { machine_path: MachinePath, container_path: ContainerPath, cgroup_permissions: string, };

export type DeviceReservation = { driver: string | null, count: number | null, device_ids: Array<string>, capabilities: Array<Array<string>>, options: { [key in string]: string }, };

export type DockerVolume = { id: DockerVolumeId, options: { [key in string]: string }, labels: { [key in string]: string }, 
/**
 * Current storage kind and Provisioned Volume usage evidence.
 */
storage: DockerVolumeStorageObservation, };

export type DockerVolumeId = { machine_id: MachineId, name: DockerVolumeName, };

export type DockerVolumeName = string;

export type DockerVolumeStorageObservation = { "kind": "plain", 
/**
 * Docker driver reported for the ordinary Volume.
 */
driver: string, } | { "kind": "provisioned", 
/**
 * Current ZFS dataset mountpoint.
 */
mountpoint: MachinePath, 
/**
 * Current ZFS dataset byte bound.
 */
bound_bytes: number, 
/**
 * Current referenced ZFS dataset bytes.
 */
used_bytes: number, };

export type ExecutionError = { "type": "machine", action: MachineAction, error: RpcError, } | { "type": "health", container_id: ContainerId, failure: HealthFailure, } | { "type": "dependency_health", dependency: QualifiedService, failure: DependencyHealthFailure, } | { "type": "hook", container_id: ContainerId, failure: HookFailure, } | { "type": "cancelled" };

export type ExtraHost = string;

export type FailedOperation<E> = { "type": "operation", operation: DeployOperation, error: E, } | { "type": "replacement_health", operation: ReplacementOperation, error: E, compensation: ReplacementCompensation<E>, };

export type HealthFailure = { "type": "cancelled" } | { "type": "timed_out" } | { "type": "runtime", observation: ContainerRuntimeObservation, };

export type HealthObservation = "not_configured" | "starting" | "healthy" | "unhealthy" | string;

export type HealthcheckCommand = [string, ...string[]];

export type HealthcheckSpec = { "state": "disabled" } | { "state": "configured" } & ConfiguredHealthcheck;

export type HookContainer = ContainerObservation;

export type HookFailure = { "type": "cancelled", stop_error: RpcError | null, } | { "type": "timed_out", stop_error: RpcError | null, } | { "type": "exit", code: number, };

export type HostBind = { "kind": "all" } | { "kind": "address", address: string, } | { "kind": "prefix", prefix: string, };

export type HttpProtocol = "http" | "https";

export type IngressHost = string;

export type IngressHostname = { "kind": "cluster_domain", label: ClusterDomainLabel | null, } | { "kind": "explicit", hostname: IngressHost, };

export type IngressProxyFragment = string;

export type JsonValue = number | string | boolean | Array<JsonValue> | { [key in string]: JsonValue } | null;

export type LocalMachineRemoved = { reset_warning: string | null, };

export type LogDriver = { name: string, options: { [key in string]: string }, };

export type Machine = { id: MachineId, name: MachineName, subnet: MachineSubnet, public_key: WireGuardPublicKey, public_ip: string | null, advertised_endpoints: Array<AdvertisedEndpoint>, runtime: MachineRuntime, };

export type MachineAction = "CreateContainer" | "StartContainer" | "InspectContainer" | "StopContainer" | "RemoveContainer" | "RemoveVolume";

export type MachineFailure<E> = { machine_id: MachineId, error: E, };

export type MachineId = string & { readonly __brand: "MachineId" };

export type MachineName = string;

export type MachineObservation = { machine: Machine, membership: MembershipObservation, 
/**
 * Current storage evidence, absent when this observer could not obtain it.
 */
storage: MachineStorageObservation | null, selected_endpoint: SelectedEndpoint | null, 
/**
 * Entry-local RTT. `ListMachines` omits it; Runtime Watch may include it.
 */
rtt: RttStatistics | null, };

export type MachinePath = string;

export type MachineRuntime = { daemon_version: string, docker_version: string, hostname: string, architecture: string, os_pretty_name: string, kernel_version: string, };

export type MachineStorageObservation = { "state": "stateless" } | { "state": "ready" } | { "state": "pool", 
/**
 * Current ZFS Pool size in bytes.
 */
size_bytes: number, 
/**
 * Current allocated ZFS Pool bytes.
 */
used_bytes: number, 
/**
 * Current free ZFS Pool bytes.
 */
free_bytes: number, };

export type MachineSubnet = string;

export type MachineSuccess<T> = { machine_id: MachineId, value: T, };

export type MachineTarget = string;

export type MembershipObservation = "unknown" | "up" | "suspect" | "down" | string;

export type ObservationKind = "container" | "volume";

export type ObservedDataLoss = { data_loss: Array<DataLoss>, };

export type OperationPhase = { "type": "starting" } | { "type": "creating_container" } | { "type": "starting_container" } | { "type": "waiting_for_health", container_id: ContainerId, health: HealthObservation | null, elapsed_ms: number, deadline_ms: number, } | { "type": "waiting_for_hook", container_id: ContainerId, elapsed_ms: number, deadline_ms: number, } | { "type": "stopping_container" } | { "type": "removing_container" } | { "type": "removing_volume" } | { "type": "compensating" };

export type OperationRow = { 
/**
 * Zero-based index in the Deploy Plan.
 */
index: number, 
/**
 * Machine this operation targets.
 */
machine_id: MachineId, 
/**
 * Human-facing Machine Name when known from the snapshot.
 */
machine_name: MachineName | null, 
/**
 * Planned operation.
 */
operation: DeployOperation, 
/**
 * Container display name when known.
 */
display_name: string | null, 
/**
 * Service Name when known from the spec or snapshot.
 */
service_name: ServiceName | null, 
/**
 * Current status of this row.
 */
status: OperationStatus, };

export type OperationStatus = { "type": "pending" } | { "type": "running", phase: OperationPhase, } | { "type": "completed" } | { "type": "failed", error: ExecutionError, } | { "type": "unexecuted" };

export type PartialResult<T, E> = { successes: Array<MachineSuccess<T>>, failures: Array<MachineFailure<E>>, 
/**
 * Targets selected by the entry Machine that produced no terminal response.
 */
omissions: Array<MachineId>, };

export type PidMode = string;

export type Placement = { 
/**
 * Machine Targets. An empty list remains every eligible Machine.
 */
machines: Array<MachineTarget>, };

export type PlanOptions = { 
/**
 * Recreate containers even when the resolved spec matches.
 */
force_recreate: boolean, 
/**
 * Skip waiting on container health after start or replace.
 */
skip_health_monitor: boolean, 
/**
 * Caller-supplied entropy keeps the planner pure while varying equal-priority placement.
 */
placement_seed: number, 
/**
 * Service Names this command applies. Empty means full reconciliation.
 */
selected: Array<ServiceAttempt>, };

export type PortPublication = { "mode": "ingress", hostname: IngressHostname, load_balancer_port: number, container_port: number, http_protocol: HttpProtocol, } | { "mode": "host", bind: HostBind, published_port: number, container_port: number, transport_protocol: TransportProtocol, };

export type PreDeployCommand = [string, ...string[]];

export type PreDeployHook = { command: PreDeployCommand, environment: { [key in string]: string }, privileged: boolean | null, timeout_millis: number | null, user: string | null, };

export type PreservedVolume = { 
/**
 * Machine-local Docker Volume identity.
 */
id: DockerVolumeId, 
/**
 * Machine Name from this observer's snapshot when known.
 */
machine_name: MachineName | null, };

export type ProjectName = string;

export type ProvisionedVolumeMaximumBytes = number;

export type PruneRefusal = "incomplete_snapshot" | "selected_services" | "filtered_profiles" | "guessed_project_name";

export type PullPolicy = "always" | "missing" | "never";

export type QualifiedService = string;

export type RegisterRequest = { name: MachineName, storage: StorageChoice, public_key: WireGuardPublicKey, public_ip: string | null, advertised_endpoints: Array<AdvertisedEndpoint>, runtime: MachineRuntime, };

export type Registered = { assigned_machine: Machine, visible_peers: Array<Machine>, target_versions: { [key in string]: number }, };

export type RemoveVolumesRequest = { volumes: Array<DockerVolumeId>, 
/**
 * Force-remove an in-use Docker Volume. Defaults to false.
 */
force: boolean, };

export type ReplacementCompensation<E> = { "type": "start_first", stop_new_container: StopAttempt<E>, } | { "type": "stop_first", stop_new_container: StopAttempt<E>, restart_old_container: RestartAttempt<E>, };

export type ReplacementOperation = { 
/**
 * Machine that hosts both containers.
 */
machine_id: MachineId, 
/**
 * Container being replaced.
 */
old_container_id: ContainerId, 
/**
 * Spec for the replacement container.
 */
spec: ResolvedServiceSpec, 
/**
 * Skip waiting on container health after the replacement starts.
 */
skip_health_monitor: boolean, };

export type RequestedServiceSpec = { name: ServiceName, mode: ServiceMode, container: ServiceContainerSpec, placement: Placement, ports: Array<PortPublication>, volumes: Array<ServiceVolume>, mounts: Array<ServiceMount>, configs: Array<ConfigSpec>, pre_deploy: PreDeployHook | null, ingress_proxy_fragment: IngressProxyFragment | null, update: UpdateConfig, };

export type ResolvedServiceSpec = { service_id: ServiceId, name: ServiceName, mode: ServiceMode, container: ServiceContainerSpec, placement: Placement, ports: Array<PortPublication>, volumes: Array<ResolvedServiceVolume>, mounts: Array<ServiceMount>, configs: Array<ConfigSpec>, pre_deploy: PreDeployHook | null, ingress_proxy_fragment: IngressProxyFragment | null, update: ResolvedUpdateConfig, };

export type ResolvedServiceVolume = { reference: ServiceVolumeReference, source: ResolvedVolumeSource, };

export type ResolvedUpdateConfig = { order: UpdateOrder, monitor_millis: number | null, };

export type ResolvedVolumeSource = (Extract<VolumeSource, { kind: "ordinary" | "provisioned" }> & { scope: ScopedVolumeSource }) | (Exclude<VolumeSource, { kind: "ordinary" | "provisioned" }> & { scope: null });

export type RestartAttempt<E> = { "type": "not_attempted" } | { "type": "restarted" } | { "type": "failed", error: E, };

export type RestartPolicy = { "name": "no" } | { "name": "always" } | { "name": "unless-stopped" } | { "name": "on-failure", maximum_retry_count: number | null, };

export type RpcError = { code: RpcErrorCode, message: string, details: JsonValue, };

export type RpcErrorCode = "invalid_argument" | "not_found" | "ambiguous" | "unsupported" | "unavailable" | "conflict" | "internal" | "unauthenticated" | string;

export type RttStatistics = { median_ns: number, population_stddev_ns: number, };

export type RuntimeWatchIncompleteIds = { machines: Array<MachineId>, containers: Array<ContainerId>, volumes: Array<DockerVolumeId>, certificates: Array<IngressHost>, };

export type RuntimeWatchView = { services: Array<ServiceObservation>, machines: Array<MachineObservation>, containers: Array<ContainerObservation>, volumes: Array<DockerVolume>, certificates: Array<CertificateObservation>, 
/**
 * Hosted DNS hostname only; never the renewal token or endpoint.
 */
hosted_dns_hostname: string | null, incomplete_ids: RuntimeWatchIncompleteIds, 
/**
 * Freshness of the entry-local membership/RTT sample. Not Cluster truth.
 */
observed_at: string, };

export type ScopedVolumeSource = { project: ProjectName, logical_name: DockerVolumeName, };

export type SelectedEndpoint = string;

export type ServiceAttempt = { 
/**
 * Service Name to apply from `DeployIntent.target`.
 */
name: ServiceName, };

export type ServiceContainer = ContainerObservation;

export type ServiceContainerSpec = { config_mounts: Array<ConfigMount>, image: string, command: Array<string>, entrypoint: Array<string>, environment: { [key in string]: string }, 
/**
 * User Docker labels, excluding Ployz's reserved management namespace.
 */
labels: ContainerLabels, 
/**
 * The container's UTS hostname, with no Ployz identity or routing meaning.
 */
hostname: ContainerHostname | null, 
/**
 * Container-local Docker `/etc/hosts` entries.
 */
extra_hosts: Array<ExtraHost>, cap_add: Array<string>, cap_drop: Array<string>, healthcheck: HealthcheckSpec | null, pull_policy: PullPolicy, init: boolean | null, user: string | null, working_directory: ContainerPath | null, tty: boolean, open_stdin: boolean, privileged: boolean, pid_mode: PidMode | null, log_driver: LogDriver | null, resources: ContainerResources, stop_timeout_secs: number | null, sysctls: { [key in string]: string }, restart: RestartPolicy, };

export type ServiceId = string & { readonly __brand: "ServiceId" };

export type ServiceMode = { "mode": "replicated", replicas: number, } | { "mode": "global" };

export type ServiceMount = { 
/**
 * Service-local Volume Reference to mount.
 */
volume: ServiceVolumeReference, 
/**
 * Absolute path inside the container.
 */
target: ContainerPath, 
/**
 * Mount the source read-only.
 */
read_only: boolean, 
/**
 * Disable Docker's initial copy into a named Volume for this mount.
 */
no_copy: boolean, 
/**
 * Mount only this Volume subdirectory.
 */
subpath: string | null, };

export type ServiceName = string;

export type ServiceObservation = { identity: QualifiedService, service_id: ServiceId, containers: Array<ServiceContainer>, hook_containers: Array<HookContainer>, };

export type ServiceVolume = { reference: ServiceVolumeReference, source: VolumeSource, };

export type ServiceVolumeReference = string;

export type StopAttempt<E> = { "type": "stopped" } | { "type": "failed", error: E, };

export type StopContainerPurpose = "lifecycle" | "free_host_ports";

export type StorageChoice = "none" | "zfs";

export type TransportProtocol = "tcp" | "udp";

export type Ulimit = { soft: number, hard: number, };

export type UpdateConfig = { 
/**
 * Absence means derive the order from the deploy snapshot.
 */
order: UpdateOrder | null, monitor_millis: number | null, };

export type UpdateOrder = "start_first" | "stop_first";

export type VolumeDriver = { name: string, options: { [key in string]: string }, };

export type VolumeRemoval = { id: DockerVolumeId, outcome: VolumeRemovalOutcome, };

export type VolumeRemovalOutcome = { "status": "removed" } | { "status": "failed", error: RpcError, } | { "status": "omitted" };

export type VolumeSource = { "kind": "bind", machine_path: MachinePath, create_machine_path: boolean, propagation: BindPropagation | null, recursive: BindRecursive | null, } | { "kind": "external", name: DockerVolumeName, } | { "kind": "ordinary", name: DockerVolumeName, driver: VolumeDriver, labels: { [key in string]: string }, } | { "kind": "provisioned", 
/**
 * Logical declaration name; immutable scoped views expose the physical name.
 */
name: DockerVolumeName, 
/**
 * Required positive storage maximum.
 */
maximum_bytes: ProvisionedVolumeMaximumBytes, 
/**
 * Labels applied when the Docker Volume is created.
 */
labels: { [key in string]: string }, } | { "kind": "tmpfs", size_bytes: number | null, mode: number | null, options: Array<Array<string>>, };

export type VolumeToCreate = { 
/**
 * Machine where the container operation will ensure the Volume.
 */
machine_id: MachineId, 
/**
 * Human-facing Machine Name from this observer's snapshot when known.
 */
machine_name: MachineName | null, 
/**
 * Physical Docker Volume Name that is currently absent on the Machine.
 */
name: DockerVolumeName, 
/**
 * Positive Provisioned Volume bound; absent for an ordinary named Volume.
 */
maximum_bytes: ProvisionedVolumeMaximumBytes | null, };

export type WireGuardPublicKey = Array<number>;

