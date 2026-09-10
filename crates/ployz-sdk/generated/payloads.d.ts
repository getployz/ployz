// Generated from the Rust wire types by `cargo test -p ployz --test sdk_payloads`.
// Do not edit.

export type AdvertisedEndpoint = string;

export type AuthoredServiceConfig = { version: 2, name: string, source: ServiceSource, preDeployCommand: string | null, startCommand: string | null, healthcheck: ServiceHealthcheck, restartPolicy: ServiceRestartPolicy, maxRetries: number, cron: string | null, replicas: number, cpuLimit: number | null, memLimit: number | null, privateDns: ServiceName, routes: Array<ServiceRoute>, managedHostname: ServiceManagedHostname | null, build: ServiceBuildConfig, };

export type BindPropagation = "private" | "rprivate" | "shared" | "rshared" | "slave" | "rslave";

export type BindRecursive = "disabled" | "writable" | "readonly";

export type ByteQuantity = number;

export type CapabilityName = string;

export type CertificateAvailability = "available" | "pending" | "failure" | "unknown" | string;

export type CertificateBackoff = { failure_kind: CertificateFailureKind, next_attempt_at: string, failures: number, };

export type CertificateFailureKind = "does_not_resolve" | "resolves_elsewhere" | "authority" | string;

export type CertificateObservation = { hostname: IngressHost, status: CertificateAvailability, last_error: string | null, backoff: CertificateBackoff | null, };

export type ChangeKind = "add" | "update" | "remove";

export type ChangeSetInput = { working: ReviewStateProjection, saved: ReviewSavedProjection, applied: ReviewStateProjection, nodeIntroductions: ReviewStateProjection, runtimeObserved: ReviewStateProjection | null, runtimeObservations?: ReviewRuntimeObservations, };

export type ClusterDomainLabel = string;

export type ClusterTeardown = { destroyed_projects: Array<ProjectName>, machines: PartialResult<LocalMachineRemoved, RpcError>, pairing_revoked: boolean, };

export type CompiledEnvironmentIntent = { nodeSnapshots: Array<CompiledEnvironmentNode>, variableProducers: Array<SavedVariableProducer>, };

export type CompiledEnvironmentNode = { environmentId: string, nodeId: string, nodeLineageId: string, encryptedRegistryUsername?: EncryptedSecretValue, encryptedRegistrySecret?: EncryptedSecretValue, nodeType: EnvironmentNodeType, configVersion: number, config: CompiledNodeConfig, };

export type CompiledNodeConfig = ServiceConfig | VariableGroupConfig | VolumeConfig;

export type ComposePruneRefusal = "filtered_profiles" | "guessed_project_name";

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
 * Effective Docker check (including image inheritance), or the Machine HTTP probe.
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

export type DependencyCondition = "service_started" | "service_healthy";

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
options: PlanOptions, dependencies: { [key in ServiceName]: Array<ServiceDependency> }, service_profiles: { [key in ServiceName]: Array<string> }, requested_profiles: Array<string>, compose_refusal: ComposePruneRefusal | null, };

export type DeployOperation = { "type": "prepare_volumes",
/**
 * Machine that owns the local Volumes.
 */
machine_id: MachineId,
/**
 * Assigned storage requirements, including reused Volumes, prepared as one batch.
 */
specs: Array<ServiceStorageSpec>, } | { "type": "wait_healthy", machine_id: MachineId, dependent: QualifiedService, dependency: QualifiedService, } | { "type": "run_container", machine_id: MachineId, spec: ResolvedServiceSpec, skip_health_monitor: boolean, } | { "type": "stop_container", machine_id: MachineId, container_id: ContainerId, purpose: StopContainerPurpose, } | { "type": "remove_container", machine_id: MachineId, container_id: ContainerId, } | { "type": "replace_container" } & ReplacementOperation | { "type": "stop_hook", machine_id: MachineId, container_id: ContainerId, } | { "type": "run_hook", machine_id: MachineId, spec: ResolvedServiceSpec, old_hook_containers: Array<[MachineId, ContainerId]>, } | { "type": "remove_volume", id: DockerVolumeId, };

export type DeployOutcome<E> = { "type": "success", completed: Array<DeployOperation>, } | { "type": "failed", completed: Array<DeployOperation>, failed: FailedOperation<E>, unexecuted: Array<DeployOperation>, };

export type DeployPreview = {
/**
 * Capacity budget for every Machine receiving provisioned storage.
 */
storage: Array<MachineStorageBudget>,
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
 * Machines during preparation or Volume Ensure. These are informational;
 * provisioned storage preparation appears separately in `operations`.
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

export type DeployWarning = { "type": "storage_headroom",
/**
 * Machine with limited remaining capacity.
 */
machine_id: MachineId,
/**
 * Bytes left after preparation and the OS reserve.
 */
remaining_bytes: number, } | { "type": "unbudgeted_disk_usage" } | { "type": "observation_failed", kind: ObservationKind, machine_id: MachineId, message: string, } | { "type": "observation_omitted", kind: ObservationKind, machine_id: MachineId, } | { "type": "storage_observation_unknown",
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

export type EncryptedSecretValue = { version: 1, iv: string, tag: string, ciphertext: string, };

export type EnrollmentAssignment = {
/**
 * Retry identity inputs, excluding assigned subnet and runtime observations.
 */
request: RegisterRequest,
/**
 * IPv4 pool from the observed Cluster configuration.
 */
network: string,
/**
 * Durable Machine identity and selected subnet to publish.
 */
machine: Machine, };

export type EnrollmentSnapshot = {
/**
 * IPv4 pool from the observed Cluster configuration.
 */
network: string,
/**
 * Observed Machines; absence does not prove an assignment is free.
 */
machines: Array<Machine>,
/**
 * Entry Machine store versions to carry into publication and Join.
 */
target_versions: { [key in string]: number }, };

export type EnvSource = { kind: 'variable_group', resourceId: string, resourceName: string, variableGroupId: string, key: string, };

export type EnvironmentNodeType = "service" | "variable_group" | "volume";

export type ExecutionError = { "type": "machine", action: MachineAction, error: RpcError, } | { "type": "health", container_id: ContainerId, failure: HealthFailure, } | { "type": "dependency_health", dependency: QualifiedService, failure: DependencyHealthFailure, } | { "type": "hook", container_id: ContainerId, failure: HookFailure, } | { "type": "cancelled" };

export type ExtraHost = string;

export type FailedOperation<E> = { "type": "operation", operation: DeployOperation, error: E, } | { "type": "replacement_health", operation: ReplacementOperation, error: E, compensation: ReplacementCompensation<E>, };

export type HealthFailure = { "type": "cancelled" } | { "type": "timed_out" } | { "type": "runtime", observation: ContainerRuntimeObservation, };

export type HealthObservation = "not_configured" | "starting" | "healthy" | "unhealthy" | string;

export type HealthcheckCommand = [string, ...string[]];

export type HealthcheckSpec = { "state": "disabled" } | { "state": "configured" } & ConfiguredHealthcheck | { "state": "http" } & HttpHealthcheck;

export type HookContainer = ContainerObservation;

export type HookFailure = { "type": "cancelled", stop_error: RpcError | null, } | { "type": "timed_out", stop_error: RpcError | null, } | { "type": "exit", code: number, };

export type HostBind = { "kind": "all" } | { "kind": "address", address: string, } | { "kind": "prefix", prefix: string, };

export type HttpHealthcheck = { path: string, port: number, timeout_seconds: number, };

export type HttpProtocol = "http" | "https";

export type IngressHost = string;

export type IngressHostname = { "kind": "cluster_domain", label: ClusterDomainLabel | null, } | { "kind": "explicit", hostname: IngressHost, };

export type IngressProxyFragment = string;

export type InitialMachinePolicy = {
/**
 * Operator classifications used by placement constraints.
 */
labels: { [key in MachineLabelKey]: MachineLabelValue },
/**
 * Whether to admit new Builds; revocation preserves existing work.
 */
accepts_builds: boolean,
/**
 * Whether to admit new application Services; revocation preserves existing work.
 */
accepts_services: boolean,
/**
 * Whether to admit the trusted Ingress Proxy; revocation preserves existing work.
 */
accepts_ingress: boolean, };

export type JsonValue = number | string | boolean | Array<JsonValue> | { [key in string]: JsonValue } | null;

export type LocalMachineRemoved = { reset_warning: string | null, };

export type LogDriver = { name: string, options: { [key in string]: string }, };

export type Machine = {
/**
 * Operator classifications used by placement constraints.
 */
labels: { [key in MachineLabelKey]: MachineLabelValue },
/**
 * Whether to admit new Builds; revocation preserves existing work.
 */
accepts_builds: boolean,
/**
 * Whether to admit new application Services; revocation preserves existing work.
 */
accepts_services: boolean,
/**
 * Whether to admit the trusted Ingress Proxy; revocation preserves existing work.
 */
accepts_ingress: boolean, id: MachineId, name: MachineName, subnet: MachineSubnet, public_key: WireGuardPublicKey, public_ip: string | null, advertised_endpoints: Array<AdvertisedEndpoint>, runtime: MachineRuntime, };

export type MachineAction = "PrepareVolumes" | "CreateContainer" | "StartContainer" | "InspectContainer" | "StopContainer" | "RemoveContainer" | "RemoveVolume";

export type MachineFailure<E> = { machine_id: MachineId, error: E, };

export type MachineId = string & { readonly __brand: "MachineId" };

export type MachineLabelKey = string;

export type MachineLabelValue = string;

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

export type MachineStorageBudget = {
/**
 * Durable identity of the Machine selected by this plan.
 */
machine_id: MachineId,
/**
 * Human-facing name from the same observation.
 */
machine_name: MachineName,
/**
 * Aggregate capacity for the selected provisioned Volumes on this Machine.
 */
budget: StorageBudget, };

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
 * Empty adds no selector restriction.
 */
constraints: Array<PlacementConstraint>, };

export type PlacementConstraint = string;

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

export type PublicationBasis = { "kind": "no_saved_state" } | { "kind": "saved_revision", savedStateSnapshotId: string, };

export type PullPolicy = "always" | "missing" | "never";

export type QualifiedService = string;

export type RegisterRequest = {
/**
 * Durable identity of the joining Machine.
 */
machine_id: MachineId,
/**
 * Client-selected subnet, required for Register publication.
 * Allocation policy callers omit it before selecting an assignment.
 */
assigned_subnet: MachineSubnet | null,
/**
 * Complete policy committed in the first Machine assignment, before participation.
 */
initial_policy: InitialMachinePolicy, name: MachineName, storage: StorageChoice, public_key: WireGuardPublicKey, public_ip: string | null, advertised_endpoints: Array<AdvertisedEndpoint>, runtime: MachineRuntime, };

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

export type ResolveVariablesInput = { parts: Array<ValuePart>, selfOwnerId: string, producers: Array<VariableProducer>, };

export type ResolveVariablesResult = { "status": "resolved", value: string, secret: boolean, warnings: Array<TemplateWarning>, } | { "status": "cycle", path: Array<string>, };

export type ResolvedServiceSpec = { service_id: ServiceId, name: ServiceName, mode: ServiceMode, container: ServiceContainerSpec, placement: Placement, ports: Array<PortPublication>, volumes: Array<ResolvedServiceVolume>, mounts: Array<ServiceMount>, configs: Array<ConfigSpec>, pre_deploy: PreDeployHook | null, ingress_proxy_fragment: IngressProxyFragment | null, update: ResolvedUpdateConfig, };

export type ResolvedServiceVolume = { reference: ServiceVolumeReference, source: ResolvedVolumeSource, };

export type ResolvedUpdateConfig = { order: UpdateOrder, monitor_millis: number | null, };

export type ResolvedVolumeSource = (Extract<VolumeSource, { kind: "ordinary" | "provisioned" }> & { scope: ScopedVolumeSource }) | (Exclude<VolumeSource, { kind: "ordinary" | "provisioned" }> & { scope: null });

export type ResolverValue = { "kind": "literal", value: string, } | { "kind": "secret", value: string, } | { "kind": "template", parts: Array<ValuePart>, };

export type RestartAttempt<E> = { "type": "not_attempted" } | { "type": "restarted" } | { "type": "failed", error: E, };

export type RestartPolicy = { "name": "no" } | { "name": "always" } | { "name": "unless-stopped" } | { "name": "on-failure", maximum_retry_count: number | null, };

export type ReviewAggregateDiscardPlan = { nodes: Array<ReviewAggregateNodePlan>, savedCommand: ReviewSavedDiscardCommand | null, };

export type ReviewAggregateNodePlan = { node: ReviewNodeIdentity, working: ReviewWorkingNodePlan, };

export type ReviewChangeSet = { unsaved: ReviewChangeSlice, pending: ReviewChangeSlice, drift: ReviewChangeSlice, discardAllPlan: ReviewAggregateDiscardPlan, };

export type ReviewChangeSlice = { provenance: ReviewProvenance, groups: Array<ReviewNodeChange>, lifecycleCount: number, settingCount: number, totalCount: number, discardPlans: ReviewDiscardPlans, };

export type ReviewDiscardPlans = { nodes: Array<ReviewNodeDiscardPlan>, settings: Array<ReviewSettingDiscardPlan>, };

export type ReviewLifecycleChange = { id: string, owner: ReviewLifecycleOwner, kind: ReviewLifecycleKind, };

export type ReviewLifecycleKind = "create" | "update" | "delete" | "none";

export type ReviewLifecycleOwner = { node: ReviewNodeIdentity, };

export type ReviewNodeChange = { id: string, node: ReviewNodeIdentity, presence: ReviewNodePresence, lifecycle: ReviewLifecycleChange, settings: Array<ReviewSettingChange>, discardPlan: ReviewNodeDiscardPlan | null, };

export type ReviewNodeDiscardKind = "restore" | "delete";

export type ReviewNodeDiscardPlan = { kind: ReviewNodeDiscardKind, node: ReviewNodeIdentity, } & ({ "target": "working" } | { "target": "saved", basis: ReviewSavedBasis, });

export type ReviewNodeIdentity = { type: EnvironmentNodeType, id: string, };

export type ReviewNodePresence = { baseline: ReviewPresence, target: ReviewPresence, };

export type ReviewNodeProjection = { node: ReviewNodeIdentity, config: CompiledNodeConfig | null, };

export type ReviewPresence = "present" | "absent";

export type ReviewProvenance = { baseline: { role: Exclude<ReviewRole, 'node_introduction'>; token: string }, target: { role: Exclude<ReviewRole, 'node_introduction'>; token: string }, };

export type ReviewRole = "working" | "saved" | "applied" | "runtime_observation" | "node_introduction";

export type ReviewRuntimeObservations = { token: string, presence?: Array<ReviewRuntimePresence>, settings: Array<ReviewRuntimeSetting>, };

export type ReviewRuntimePresence = { node: ReviewNodeIdentity, applied: ReviewPresence, observed: ReviewPresence, };

export type ReviewRuntimeSetting = { node: ReviewNodeIdentity, setting: string, label: string, appliedValue: string | null, observedValue: string | null, };

export type ReviewSavedBasis = { "kind": "saved_revision", savedStateSnapshotId: string, };

export type ReviewSavedCommandKind = "discard";

export type ReviewSavedDiscardCommand = { kind: ReviewSavedCommandKind, basis: ReviewSavedBasis, operations: Array<ReviewSavedNodeOperation>, };

export type ReviewSavedNodeOperation = { kind: ReviewSavedOperationKind, nodeType: EnvironmentNodeType, nodeId: string, };

export type ReviewSavedOperationKind = "node";

export type ReviewSavedProjection = { "kind": "no_saved_state", token: string, nodes: Array<ReviewNodeProjection>, } | { "kind": "saved_revision", token: string, nodes: Array<ReviewNodeProjection>, savedStateSnapshotId: string, };

export type ReviewSettingChange = { id: string, owner: ReviewSettingOwner,
/**
 * Runtime adapter labels are retained. Authored labels are rendered by the consumer.
 */
label: string | null, kind: ReviewSettingKind, baselineValue: JsonValue, targetValue: JsonValue, baselineSource: ReviewSource | null, discardPlan: ReviewSettingDiscardPlan | null, };

export type ReviewSettingDiscardKind = "restore_setting";

export type ReviewSettingDiscardPlan = { kind: ReviewSettingDiscardKind, owner: ReviewSettingOwner, config: CompiledNodeConfig, } & ({ "target": "working" } | { "target": "saved", basis: ReviewSavedBasis, });

export type ReviewSettingKind = "add" | "update" | "remove" | "drift";

export type ReviewSettingOwner = { node: ReviewNodeIdentity, setting: string, };

export type ReviewSource = { role: ReviewRole, token: string, };

export type ReviewStateProjection = { token: string, nodes: Array<ReviewNodeProjection>, };

export type ReviewWorkingNodePlan = { kind: ReviewNodeDiscardKind, target: ReviewWorkingTarget, node: ReviewNodeIdentity, };

export type ReviewWorkingTarget = "working";

export type RpcError = { code: RpcErrorCode, message: string, details: JsonValue, };

export type RpcErrorCode = "invalid_argument" | "not_found" | "ambiguous" | "unsupported" | "unavailable" | "conflict" | "internal" | "unauthenticated" | string;

export type RttStatistics = { median_ns: number, population_stddev_ns: number, };

export type RuntimeFailureKind = "machine" | "health" | "dependency_health" | "hook" | "cancelled";

export type RuntimeOutcomeProjection = {
/**
 * Sanitized whole-attempt counts and disposition.
 */
summary: RuntimeOutcomeSummary,
/**
 * Services whose every planned operation completed, ordered by name.
 */
confirmedServices: Array<ServiceName>, };

export type RuntimeOutcomeSummary = { "type": "success",
/**
 * Number of completed operations.
 */
completed: number, } | { "type": "failed",
/**
 * Number of completed operations.
 */
completed: number,
/**
 * Number of operations never attempted, excluding the failed operation.
 */
unexecuted: number,
/**
 * The failed operation's sanitized failure kind.
 */
reason: RuntimeFailureKind, };

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

export type SavedDiscardCommand = { kind: 'discard', basis: ReviewSavedBasis, operations: Array<SavedDiscardOperation>, };

export type SavedDiscardOperation = { "kind": "node", nodeType: EnvironmentNodeType, nodeId: string, } | { "kind": "setting", nodeType: 'service', nodeId: string, setting: string, };

export type SavedEnvironmentIntent = { version: 1, environmentSlug: string, services: Array<SavedServiceIntent>, variableGroups: Array<SavedVariableGroupIntent>, volumes: Array<SavedVolumeIntent>, };

export type SavedServiceIntent = { id: string, lineageId: string, slug: string, variables: Array<SavedVariableIntent>, variableGroupAttachments: Array<VariableGroupAttachment>, volumeAttachments: Array<VolumeAttachment>, config: AuthoredServiceConfig, encryptedRegistryUsername: EncryptedSecretValue | null, encryptedRegistrySecret: EncryptedSecretValue | null, };

export type SavedVariableGroupIntent = { resourceId: string, resourceLineageId: string, variableGroupId: string, variableGroupLineageId: string, slug: string, name: string, variables: Array<SavedVariableIntent>, };

export type SavedVariableIntent = { id: string, key: string, description: string | null, exported: boolean, valueFingerprint: string, value: SavedVariableValue, };

export type SavedVariableProducer = { ownerScope: 'service' | 'variable_group', ownerId: string, ownerLineageId: string, key: string, value: SavedVariableValue, };

export type SavedVariableValue = { "kind": "literal", value: string, } | { "kind": "template", parts: Array<ValuePart>, } | { "kind": "secret",
/**
 * Absent in the browser-readable authored document. Cloud keeps the
 * ciphertext privately and captures it into each immutable publication.
 */
encryptedValue: EncryptedSecretValue | null, };

export type SavedVolumeIntent = { resourceId: string, resourceLineageId: string, name: string, };

export type ScopedVolumeSource = { project: ProjectName, logical_name: DockerVolumeName, };

export type SelectedEndpoint = string;

export type ServiceAttempt = {
/**
 * Service Name to apply from `DeployIntent.target`.
 */
name: ServiceName, };

export type ServiceBuildConfig = { builder: ServiceBuilder, dockerfilePath: string | null, watchPaths: Array<string>, };

export type ServiceBuilder = "dockerfile" | "auto";

export type ServiceConfig = { env: { [key in string]: ServiceEnvValue }, mounts: Array<ServiceDeployMount>, variableGroupAttachments: Array<VariableGroupAttachment>, version: 2, name: string, source: ServiceSource, preDeployCommand: string | null, startCommand: string | null, healthcheck: ServiceHealthcheck, restartPolicy: ServiceRestartPolicy, maxRetries: number, cron: string | null, replicas: number, cpuLimit: number | null, memLimit: number | null, privateDns: ServiceName, routes: Array<ServiceRoute>, managedHostname: ServiceManagedHostname | null, build: ServiceBuildConfig, };

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

export type ServiceDependency = {
/**
 * Service that the dependent Service requires.
 */
service: ServiceName,
/**
 * Condition the dependency must satisfy before the dependent starts.
 */
condition: DependencyCondition, };

export type ServiceDeployMount = { volumeResourceId: string, volumeName: string, mountPath: string, };

export type ServiceEnvValue = { "kind": "literal", value: string, source?: EnvSource, parts?: Array<ValuePart>, } | { "kind": "secret", variableId?: string, encryptedValue?: EncryptedSecretValue, fingerprint: string, source?: EnvSource, interpolated?: boolean, };

export type ServiceGitBranch = { "type": "connected", name: string, } | { "type": "disconnected", previousName: string | null, };

export type ServiceHealthcheck = { "type": "none" } | { "type": "http", path: string, timeoutSeconds: number, };

export type ServiceId = string & { readonly __brand: "ServiceId" };

export type ServiceImageAutoUpdate = { "type": "off" } | { "type": "track-tag", tag: string, };

export type ServiceImageCredentials = { "type": "none" } | { "type": "configured", revision?: string, };

export type ServiceManagedHostname = { prefix: string, targetPort: number | null, };

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

export type ServiceRestartPolicy = 'unless-stopped' | 'always' | 'on-failure' | 'no';

export type ServiceRoute = { id: string, hostname: string, targetPort: number, };

export type ServiceSettingChange = { path: string, kind: ChangeKind, before: JsonValue, after: JsonValue, canRestore: boolean, derivedFrom?: EnvSource, };

export type ServiceSettingInput = { "field": "name", "value": string } | { "field": "source", "value": ServiceSource } | { "field": "rootDir", "value": string } | { "field": "command", "value": string } | { "field": "preDeployCommand", "value": string | null } | { "field": "startCommand", "value": string | null } | { "field": "healthcheck", "value": ServiceHealthcheck } | { "field": "healthcheckPath", "value": string } | { "field": "healthcheckTimeoutSeconds", "value": number } | { "field": "restartPolicy", "value": ServiceRestartPolicy } | { "field": "maxRetries", "value": number } | { "field": "cron", "value": string | null } | { "field": "replicas", "value": number } | { "field": "cpuLimit", "value": number | null } | { "field": "memLimit", "value": number | null } | { "field": "privateDns", "value": ServiceName } | { "field": "routes", "value": Array<ServiceRoute> } | { "field": "managedHostname", "value": ServiceManagedHostname | null } | { "field": "managedHostnameValue", "value": ServiceManagedHostname } | { "field": "managedHostnamePrefix", "value": string } | { "field": "build", "value": ServiceBuildConfig };

export type ServiceSource = { "type": "empty", version: 1, rootDir: string, } | { "type": "git", version: 2, repository: string, repositoryId: number, installationId: number, rootDir: string, branch: ServiceGitBranch, autoDeploy: boolean, waitForCi: boolean, } | { "type": "image", version: 1, image: string, autoUpdate: ServiceImageAutoUpdate, credentials: ServiceImageCredentials, };

export type ServiceStorageSpec = { placement: Placement, volumes: Array<ResolvedServiceVolume>, mounts: Array<ServiceMount>, };

export type ServiceVolume = { reference: ServiceVolumeReference, source: VolumeSource, };

export type ServiceVolumeReference = string;

export type StopAttempt<E> = { "type": "stopped" } | { "type": "failed", error: E, };

export type StopContainerPurpose = "lifecycle" | "free_host_ports";

export type StorageBudget = {
/**
 * Sum of unique bounds requested by this deployment, including reused Volumes.
 */
requested_bytes: number,
/**
 * Bounds not already committed by existing managed datasets.
 */
additional_commitment_bytes: number,
/**
 * Conservative backing-growth estimate; initial Pools include one GiB for ZFS size loss.
 * Allocation rechecks actual usable capacity, which cannot be known before Pool creation.
 */
required_growth_bytes: number,
/**
 * Free host filesystem bytes, or remaining usable capacity for a fixed Pool.
 */
available_bytes: number,
/**
 * Host filesystem bytes retained for the OS; zero for a fixed Pool.
 */
reserve_bytes: number, };

export type StorageCapacityError = { "code": "insufficient_storage",
/**
 * Additional physical backing required in bytes.
 */
required_growth_bytes: number,
/**
 * Observed available bytes before preserving the reserve.
 */
available_bytes: number,
/**
 * Host bytes retained for the operating system.
 */
reserve_bytes: number, } | { "code": "pool_cannot_grow",
/**
 * Total Pool capacity required including overhead and occupancy.
 */
required_bytes: number,
/**
 * Observed usable capacity of the fixed Pool.
 */
capacity_bytes: number, } | { "code": "storage_capacity_unknown",
/**
 * The missing or invalid evidence.
 */
message: string, } | { "code": "volume_size_conflict",
/**
 * The conflicting Volume.
 */
name: DockerVolumeName, };

export type StorageChoice = "none" | "zfs";

export type TemplateWarning = { kind: 'missing', ownerId: string | null, key: string, };

export type TransportProtocol = "tcp" | "udp";

export type Ulimit = { soft: number, hard: number, };

export type UpdateConfig = {
/**
 * Absence means derive the order from the deploy snapshot.
 */
order: UpdateOrder | null, monitor_millis: number | null, };

export type UpdateOrder = "start_first" | "stop_first";

export type ValuePart = { "kind": "text", value: string, } | { "kind": "ref", owner: ValuePartOwner, key: string, };

export type ValuePartOwner = { "scope": "self" } | { "scope": "service", lineageId: string, } | { "scope": "variable_group", lineageId: string, };

export type VariableGroupAttachment = { variableGroupId: string, sortOrder: number, };

export type VariableGroupConfig = { version: 1, name: string, variables: Array<VariableGroupConfigVariable>, };

export type VariableGroupConfigValue = { "type": "plain", value: string, } | { "type": "sealed", hasValue: true, fingerprint: string, encryptedValue?: EncryptedSecretValue, };

export type VariableGroupConfigVariable = { key: string, description: string | null, exported: boolean, value: VariableGroupConfigValue, };

export type VariableProducer = { ownerId: string, owner: ValuePartOwner, key: string, value: ResolverValue, };

export type VolumeAttachment = { volumeResourceId: string, mountPath: string, };

export type VolumeConfig = { version: 2, name: string, };

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

