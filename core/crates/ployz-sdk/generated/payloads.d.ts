// Generated from the Rust wire types by `cargo test -p ployz --test sdk_payloads`.
// Do not edit.

export type AddDomain = { environment: EnvironmentRef, service: ServiceName,
/**
 * A custom hostname; none generates one.
 */
hostname: Hostname | null,
/**
 * The container port it reaches; none follows the container's `PORT`.
 */
port: number | null, };

export type Admit = { "admit": "deploy" } & Deploy | { "admit": "retry" } & Retry | { "admit": "remove" } & Removal;

export type AdvertisedEndpoint = string;

export type AppliedVolume = { project: ProjectName, environment: EnvironmentName, volume: VolumeName, };

export type Apply = "staged" | "immediate";

export type Approval = "not_required" | "required" | { "approved": ApprovalDigest };

export type ApprovalDigest = string;

export type AttachConfig = {
/**
 * The Environment they are in.
 */
environment: EnvironmentRef,
/**
 * The Service, by name.
 */
service: ServiceName,
/**
 * The Config, by name.
 */
config: ConfigName,
/**
 * The absolute directory in the Service's containers.
 */
dir: string, };

export type AuthoredServiceConfig = { version: 2, source: ServiceSource, preDeployCommand: string | null, startCommand: string | null, healthcheck: ServiceHealthcheck, restartPolicy: ServiceRestartPolicy, maxRetries: number, replicas: number, cpuLimit: number | null, memLimit: number | null, privateDns: ServiceName, routes: Array<ServiceRoute>, managedHostnames: Array<ServiceManagedHostname>, build: ServiceBuildConfig,
/**
 * The Service Template it was created from; authoring metadata only.
 */
template?: ServiceTemplate, };

export type AuthorizedRepository = {
/**
 * Its `owner/name`, as GitHub spells it.
 */
repository: RepositoryName,
/**
 * GitHub's ID for it.
 */
repository_id: RepositoryId,
/**
 * How Cloud reads it: publicly, or through a GitHub installation.
 */
access: ServiceGitAccess,
/**
 * Its default branch.
 */
default_branch: BranchName,
/**
 * Other branches Cloud saw exist.
 */
branches: Array<BranchName>, };

export type AutoDeployed = { environment: EnvironmentId, deployment: DeploymentSummary, };

export type Automated = {
/**
 * Deployments admitted: Cloud dispatches each to a runner.
 */
admitted: Array<AutoDeployed>,
/**
 * Environments whose deploy waits for the commit's CI.
 */
waiting: Array<EnvironmentId>,
/**
 * Environments that would have deployed but can't, and why.
 */
skipped: Array<Skipped>,
/**
 * Branches the Store is closing that still run on the Servers: Cloud admits
 * their removal (`Admit { remove }`) with the runtime evidence it gathers.
 */
closing: Array<EnvironmentSummary>,
/**
 * Environments deleted: nothing of them runs on the Servers any more.
 */
removed: Array<EnvironmentSummary>,
/**
 * Pull requests whose GitHub check Cloud publishes again.
 */
checks: Array<PullRequestRef>,
/**
 * Deployments cancelled without a runner, oldest first: their Cluster is gone.
 */
cancelled: Array<DeploymentId>, };

export type Batch = {
/**
 * The Environment every command writes. A command naming another is refused.
 */
environment: EnvironmentRef,
/**
 * The commands, applied in order.
 */
commands: Array<BatchCommand>,
/**
 * Refuse with `conflict` unless Working State is at this revision before the
 * first command.
 */
expect?: Revision | null, };

export type BatchCommand = { "command": "create_service" } & CreateService | { "command": "create_volume" } & CreateVolume | { "command": "create_config" } & CreateConfig | { "command": "put_config_file" } & PutConfigFile | { "command": "remove_config_file" } & RemoveConfigFile | { "command": "rename_config" } & RenameConfig | { "command": "delete_config" } & DeleteConfig | { "command": "attach_config" } & AttachConfig | { "command": "detach_config" } & DetachConfig | { "command": "edit" } & Edit | { "command": "discard" } & Discard | { "command": "never_sync" } & NeverSync;

export type Batched = {
/**
 * One result per command.
 */
results: Array<ConfigWritten>, };

export type BindPropagation = "private" | "rprivate" | "shared" | "rshared" | "slave" | "rslave";

export type BindRecursive = "disabled" | "writable" | "readonly";

export type BranchHead = { repository_id: RepositoryId, branch: BranchName,
/**
 * The head Cloud compared from: the Store's, as [`crate::ConfigStore::branch_head`]
 * read it. Anything else is `conflict`: read it again and compare again.
 */
base: CommitSha | null,
/**
 * The head now; none once the branch was deleted.
 */
head: CommitSha | null,
/**
 * The paths `base..head` changed, when `head` is ahead of `base` and GitHub
 * listed every one. None (a force-push, diverged or long history) deploys every
 * Service that follows the branch.
 */
changed: Array<string> | null,
/**
 * The merge commits of frozen Conditional Syncs ([`crate::PendingSyncs::merged`])
 * Cloud found `head` is or descends from: this push carries those.
 */
merged: Array<CommitSha>, };

export type BranchName = string;

export type BranchNodeReason = "picked" | "used" | "parent_not_deployed";

export type BranchPicks = { preset: BranchPreset, } | { own: Array<string>, };

export type BranchPlan = { nodes: Array<BranchPlanNode>, preset: BranchPreset | null, };

export type BranchPlanNode = { lineageId: string, nodeType: EnvironmentNodeType, } & ({ "role": "own", because: BranchNodeReason, } | { "role": "live" } | { "role": "left_out" });

export type BranchPlanQuery = {
/**
 * The Environment to branch.
 */
from: EnvironmentRef,
/**
 * The nodes the Branch is for, by name: what a preset plans around.
 */
focus: Array<NodeName>,
/**
 * The nodes to copy, by name, as [`CreateBranch::copy`]; ignored with a preset.
 */
copy: Array<NodeName>,
/**
 * Plan what a preset copies around `focus` instead.
 */
preset?: BranchPreset | null, };

export type BranchPlanView = {
/**
 * The Environment it comes from.
 */
from: EnvironmentSummary,
/**
 * The preset this plan is, if any.
 */
preset: BranchPreset | null,
/**
 * The presets worth offering: "uses" only when it copies more than "only".
 */
presets: Array<BranchPreset>,
/**
 * Each node, as the Branch would have it.
 */
nodes: Array<PlannedNode>, };

export type BranchPreset = "only" | "uses" | "all";

export type BranchQuery = {
/**
 * The Branch.
 */
environment: EnvironmentRef, };

export type BranchView = {
/**
 * The Branch.
 */
environment: EnvironmentSummary,
/**
 * The Environment it was made from, in the same Project.
 */
parent: EnvironmentName,
/**
 * Whether it stays after syncing into its Parent and never closes for being idle.
 */
kept: boolean,
/**
 * What runs in each Own Copy before it first deploys.
 */
setup: Array<SetupCommand>,
/**
 * The nodes it uses live.
 */
live: Array<LiveNode>,
/**
 * How many changes a Sync into its Parent carries: the Sync view's rows
 * ticked by default.
 */
to_parent: number,
/**
 * When it closes for sitting idle, in seconds since the Unix epoch: a week
 * after its latest Deployment. None while kept, never deployed, a Parent or
 * closing already.
 */
closes_at: number | null,
/**
 * The pull request it is the PR Environment of; its Sync into a Destination
 * waits for the merge.
 */
pull_request: PullRequestRef | null, };

export type Branched = {
/**
 * The Branch now.
 */
branch: BranchView,
/**
 * Nodes staged in its Working State.
 */
staged: Array<NodeName>, };

export type BridgeEndpointCapacity = { bridge_usable_endpoints: number, bridge_attached_endpoints: number, bridge_free_endpoints: number, };

export type BuildConcurrency = number;

export type BuildConcurrencyUpdate = { "action": "keep" } | { "action": "automatic" } | { "action": "set", "value": BuildConcurrency };

export type BuildGrantEnded = {
/**
 * `sha256:` digest of the manifest the Machine verified and stored, when the
 * push completed; absent when nothing was pushed.
 */
pushed: ImageDigest | null, };

export type BuildGrantId = string & { readonly __brand: "BuildGrantId" };

export type BuildGrantMinted = {
/**
 * Handle for [`EndBuildGrantRequest`]; not secret.
 */
id: BuildGrantId,
/**
 * Secret-bearing grant for the pusher only.
 */
grant: string,
/**
 * The grant ends by itself this long after minting.
 */
expires_in_seconds: number, };

export type BuildGrantRepository = string;

export type BuildLogQuery = { deployment: DeploymentId,
/**
 * The Service it built.
 */
service: ServiceName, };

export type BuildLogView = { deployment: DeploymentId, log: string,
/**
 * The Service's name when admitted.
 */
service: ServiceName,
/**
 * The commit it builds; none when it builds from the Deployment's upload.
 */
commit: CommitSha | null, status: BuildStatus,
/**
 * Why it failed.
 */
message: string | null, };

export type BuildMethod = "dockerfile" | "railpack";

export type BuildOrder = "servers-only" | "github-then-servers" | "servers-then-github" | "github-only";

export type BuildOrderQuery = Record<symbol, never>;

export type BuildOrderView = {
/**
 * None while it is Auto.
 */
build_order: BuildOrder | null, builders: Array<Builder>, };

export type BuildStatus = "pending" | "building" | "built" | "reused" | "failed";

export type BuildView = {
/**
 * The Service's name when admitted.
 */
service: ServiceName,
/**
 * The commit it builds; none when it builds from the Deployment's upload.
 */
commit: CommitSha | null, status: BuildStatus,
/**
 * Why it failed.
 */
message: string | null, };

export type Builder = "github" | "servers";

export type ByteQuantity = number;

export type Cancel = { deployment: DeploymentId, };

export type CapabilityName = string;

export type CertificateAvailability = "available" | "pending" | "failure" | "unknown" | string;

export type CertificateBackoff = { failure_kind: CertificateFailureKind, next_attempt_at: string, failures: number, };

export type CertificateFailureKind = "does_not_resolve" | "unreachable" | "redirects_to_https" | "reaches_elsewhere" | "authority" | string;

export type CertificateHost = string;

export type CertificateMaterialChange = { "action": "set", certificate_chain_pem: string, private_key_pem: string, } | { "action": "clear" };

export type CertificateMaterialPublished = Record<symbol, never>;

export type CertificateObservation = { hostname: CertificateHost, status: CertificateAvailability, last_error: string | null, backoff: CertificateBackoff | null,
/**
 * The issued certificate was ordered through a proxy in front of this Cluster.
 */
via_proxy: boolean, };

export type Change = { "op": "set",
/**
 * The Setting.
 */
path: SettingPath,
/**
 * Its new value.
 */
value: JsonValue, } | { "op": "unset",
/**
 * The Setting.
 */
path: SettingPath, } | { "op": "patch",
/**
 * The Service, as `SERVICE`.
 */
path: SettingPath,
/**
 * Its Settings by name.
 */
value: JsonValue, };

export type ChangeKind = "add" | "update" | "remove";

export type ChangeSetInput = { working: ReviewStateProjection, applied: ReviewStateProjection, saved: ReviewStateProjection | null, submitted: ReviewStateProjection | null, nodeIntroductions: ReviewStateProjection, };

export type CheckConclusion = "success" | "neutral" | "skipped" | "failure" | "cancelled" | "timed_out" | "action_required" | "stale" | "startup_failure" | "other";

export type CheckStatus = "queued" | "in_progress" | "completed" | "other";

export type CheckSuite = { repository_id: RepositoryId, suite: number,
/**
 * The commit it checks.
 */
head: CommitSha, status: CheckStatus,
/**
 * GitHub's conclusion; read only once it completed.
 */
conclusion: CheckConclusion | null,
/**
 * When GitHub last changed it: an older result never replaces a newer one.
 */
updated: GithubTimestamp, };

export type ClusterDomain = { name: Hostname, status: ClusterDomainStatus, };

export type ClusterDomainStatus = { "kind": "setting_up" } | { "kind": "ready" } | { "kind": "no_servers" } | { "kind": "no_public_ip" } | { "kind": "port_80", addresses: Array<string>, } | { "kind": "https_down" };

export type ClusterTeardown = { destroyed_namespaces: Array<Namespace>, machines: PartialResult<LocalMachineRemoved, RpcError>, pairing_revoked: boolean, };

export type CommitRequest = { switch: Switch, name: DockerVolumeName, mirror_newest: SnapshotGuid, };

export type CommitSha = string;

export type CompiledEnvironmentIntent = { nodeSnapshots: Array<CompiledEnvironmentNode>, variableProducers: Array<SavedVariableProducer>, };

export type CompiledEnvironmentNode = { environmentId: string, nodeId: string, nodeLineageId: string, encryptedRegistryUsername?: EncryptedSecretValue, encryptedRegistrySecret?: EncryptedSecretValue, nodeType: EnvironmentNodeType, configVersion: number, config: CompiledNodeConfig, };

export type CompiledNodeConfig = ServiceConfig | VolumeConfig | ConfigNodeConfig;

export type ConditionalSync = {
/**
 * Pass to [`crate::Take::from`] to use a hint it left.
 */
id: ConditionalSyncId,
/**
 * The pull request whose merge lands it.
 */
pull_request: PullRequestNumber,
/**
 * The rows it holds.
 */
rows: Array<NamedRow>,
/**
 * Where it is.
 */
state: ConditionalSyncState, };

export type ConditionalSyncId = string;

export type ConditionalSyncState = "standing" | "frozen" | "landed";

export type ConfigAttachment = { configResourceId: string, mountDir: ContainerPath, };

export type ConfigCommand = { "command": "create_project" } & CreateProject | { "command": "rename_project" } & RenameProject | { "command": "create_environment" } & CreateEnvironment | { "command": "create_service" } & CreateService | { "command": "create_git_service" } & CreateGitService | { "command": "rename_service" } & RenameService | { "command": "remove_service" } & RemoveService | { "command": "create_volume" } & CreateVolume | { "command": "set_volume_storage" } & SetVolumeStorage | { "command": "set_volume_shared_writes" } & SetVolumeSharedWrites | { "command": "remove_volume" } & RemoveVolume | { "command": "rename_volume" } & RenameVolume | { "command": "create_config" } & CreateConfig | { "command": "put_config_file" } & PutConfigFile | { "command": "remove_config_file" } & RemoveConfigFile | { "command": "rename_config" } & RenameConfig | { "command": "delete_config" } & DeleteConfig | { "command": "attach_config" } & AttachConfig | { "command": "detach_config" } & DetachConfig | { "command": "edit" } & Edit | { "command": "publish" } & Publish | { "command": "discard" } & Discard | { "command": "admit" } & Admit | { "command": "start" } & Start | { "command": "cancel" } & Cancel | { "command": "add_domain" } & AddDomain | { "command": "set_generated_domain" } & SetGeneratedDomain | { "command": "remove_domain" } & RemoveDomain | { "command": "create_branch" } & CreateBranch | { "command": "sync" } & SyncChanges | { "command": "undo_sync" } & UndoSync | { "command": "take" } & Take | { "command": "hold_secret" } & HoldSecret | { "command": "copy_node" } & CopyNode | { "command": "never_sync" } & NeverSync | { "command": "keep_branch" } & KeepBranch | { "command": "set_build_order" } & SetBuildOrder | { "command": "set_default_environment" } & SetDefaultEnvironment | { "command": "set_branch_setup" } & SetBranchSetup | { "command": "remove_environment" } & RemoveEnvironment | { "command": "remove_project" } & RemoveProject | { "command": "set_pr_plan" } & SetPrPlan | { "command": "batch" } & Batch;

export type ConfigCommitted = {
/**
 * The open PR Environments' pull requests in the Project of the Environment it
 * wrote; none when it wrote none.
 */
checks: Array<PullRequestRef>, } & ({ "written": "project" } & ProjectCreated | { "written": "project_renamed" } & ProjectSummary | { "written": "environment" } & EnvironmentCreated | { "written": "service" } & ServiceStaged | { "written": "service_renamed" } & ServiceStaged | { "written": "service_removed" } & ServiceStaged | { "written": "volume" } & VolumeStaged | { "written": "volume_removed" } & VolumeStaged | { "written": "volume_renamed" } & VolumeStaged | { "written": "config" } & ConfigStaged | { "written": "config_removed" } & ConfigStaged | { "written": "config_renamed" } & ConfigStaged | { "written": "edited" } & Edited | { "written": "published" } & Published | { "written": "discarded" } & Discarded | { "written": "deployment" } & DeploymentSummary | { "written": "domain" } & DomainStaged | { "written": "automated" } & Automated | { "written": "branch" } & Branched | { "written": "build_order" } & BuildOrderView | { "written": "synced" } & Synced | { "written": "undone" } & Undone | { "written": "taken" } & Taken | { "written": "secret_held" } & SecretHeld | { "written": "never_synced" } & NeverSynced | { "written": "default_environment" } & EnvironmentsView | { "written": "branch_setup" } & EnvironmentsView | { "written": "environment_removed" } & Teardown<EnvironmentRemoved> | { "written": "project_removed" } & Teardown<ProjectRemoved> | { "written": "pr_plans" } & PrPlansView | { "written": "batch" } & Batched);

export type ConfigDomainEvidence = {
/**
 * The Organization's Cluster Domain, once reserved.
 */
cluster_domain: ClusterDomain | null,
/**
 * The Cluster's certificates as its Runtime Watch reported them; none when Cloud
 * couldn't observe the Cluster.
 */
certificates: Array<CertificateObservation> | null,
/**
 * Public addresses of the Servers that accept ingress.
 */
ingress_addresses: Array<string>,
/**
 * What DNS answered just now, for the hostnames a check looked up.
 */
lookups: Array<DnsLookup>,
/**
 * Hostnames the Servers publish now, as the Runtime Watch last reported them.
 * Generated prefixes avoid those of other Namespaces, and a Deploy refuses one.
 */
published?: Array<PublishedHostname>, };

export type ConfigFileName = string;

export type ConfigFileSummary = {
/**
 * Its path inside the Config.
 */
name: ConfigFileName,
/**
 * How many bytes its text is, as written, references unresolved.
 */
bytes: number,
/**
 * Its permission bits.
 */
mode: FileMode,
/**
 * The user ID that owns it.
 */
uid: number,
/**
 * The group ID that owns it.
 */
gid: number,
/**
 * The Services its text references, by name.
 */
references: Array<string>, };

export type ConfigId = string;

export type ConfigItemQuery = {
/**
 * The Environment it is in.
 */
environment: EnvironmentRef,
/**
 * Its name, or `@UUID` for one exact identity.
 */
config: ConfigRef, };

export type ConfigItemView = {
/**
 * The Environment, at the revision read.
 */
environment: EnvironmentSummary,
/**
 * The lineage its Environment copies share.
 */
lineage: ConfigId,
/**
 * Each file's text as written, references by Service name.
 */
contents: { [key in ConfigFileName]: string },
/**
 * The Services mounting it in Working State.
 */
mounts: Array<ConfigMountAt>,
/**
 * Whether a Deploy applied it.
 */
deployed: boolean,
/**
 * What the next Deploy does to it; none when it is deployed as it is.
 */
change: ReviewLifecycleKind | null,
/**
 * Its durable identity.
 */
id: ConfigId,
/**
 * Its name, which mount paths address it by.
 */
name: ConfigName,
/**
 * Its files, by path.
 */
files: Array<ConfigFileSummary>, };

export type ConfigListing = {
/**
 * The Services mounting it in Working State.
 */
mounts: Array<ConfigMountAt>,
/**
 * Whether a Deploy applied it.
 */
deployed: boolean,
/**
 * What the next Deploy does to it; none when it is deployed as it is.
 */
change: ReviewLifecycleKind | null,
/**
 * Its durable identity.
 */
id: ConfigId,
/**
 * Its name, which mount paths address it by.
 */
name: ConfigName,
/**
 * Its files, by path.
 */
files: Array<ConfigFileSummary>, };

export type ConfigMount = { config_name: string,
/**
 * Omission defaults to `/{config_name}`. Admitted specs retain the canonical target.
 */
target: ContainerPath | null, uid: number | null, gid: number | null, mode: number | null, };

export type ConfigMountAt = {
/**
 * The Service, by name.
 */
service: ServiceName,
/**
 * The absolute directory in its containers where the Config's files appear.
 */
dir: string, };

export type ConfigName = string;

export type ConfigNodeConfig = { version: 1, name: ConfigName, files: { [key in ConfigFileName]: SavedConfigFile }, };

export type ConfigQuery = { "query": "environment" } & EnvironmentQuery | { "query": "diff" } & DiffQuery | { "query": "plan" } & PlanQuery | { "query": "deployments" } & DeploymentsQuery | { "query": "deployment" } & DeploymentQuery | { "query": "numbered_deployment" } & NumberedDeploymentQuery | { "query": "build_log" } & BuildLogQuery | { "query": "services" } & ServicesQuery | { "query": "service" } & ServiceQuery | { "query": "namespace" } & NamespaceQuery | { "query": "namespaces" } & NamespacesQuery | { "query": "domains" } & DomainsQuery | { "query": "domain" } & DomainQuery | { "query": "volumes" } & VolumesQuery | { "query": "volume" } & VolumeQuery | { "query": "removals" } & RemovalsQuery | { "query": "configs" } & ConfigsQuery | { "query": "config" } & ConfigItemQuery | { "query": "branch" } & BranchQuery | { "query": "branch_plan" } & BranchPlanQuery | { "query": "build_order" } & BuildOrderQuery | { "query": "sync" } & SyncQuery | { "query": "environments" } & EnvironmentsQuery | { "query": "projects" } & ProjectsQuery | { "query": "pr_plans" } & PrPlansQuery | { "query": "pull_request" } & PullRequestQuery;

export type ConfigRef = string;

export type ConfigSpec = { name: string, content: Array<number>, };

export type ConfigStaged = {
/**
 * The Config.
 */
config: ConfigSummary,
/**
 * The Environment, at its revision after the change.
 */
environment: EnvironmentSummary,
/**
 * What waits for a Deploy: the Config as `configs.NAME`, and each mount it
 * gained or lost as `SERVICE.configs.NAME`.
 */
staged: Array<SettingPath>, };

export type ConfigSummary = {
/**
 * Its durable identity.
 */
id: ConfigId,
/**
 * Its name, which mount paths address it by.
 */
name: ConfigName,
/**
 * Its files, by path.
 */
files: Array<ConfigFileSummary>, };

export type ConfigTrusted = {
/**
 * Repositories the calling Organization may read, with the branches Cloud saw.
 */
repositories: Array<AuthorizedRepository>,
/**
 * What Cloud observed of the Organization's public domains and traffic.
 */
domains: ConfigDomainEvidence,
/**
 * Which Servers hold the Docker Volumes a Deploy would delete, when it deletes any.
 */
volumes?: VolumeObservation,
/**
 * How many Servers the Organization has, as Cloud counts them: a Deployment is
 * admitted only when one could run it. None when the caller can't count them,
 * such as the hidden local Store, which runs its Deployments itself.
 */
servers?: number,
/**
 * Whether a human must approve a publication that destroys something, and what they
 * approved.
 */
approval?: Approval, };

export type ConfigView = { "view": "environment" } & EnvironmentView | { "view": "diff" } & DiffView | { "view": "plan" } & PlanView | { "view": "deployments" } & DeploymentsView | { "view": "deployment" } & DeploymentView | { "view": "numbered_deployment" } & DeploymentView | { "view": "build_log" } & BuildLogView | { "view": "services" } & ServicesView | { "view": "service" } & ServiceView | { "view": "namespace" } & NamespaceView | { "view": "namespaces" } & NamespacesView | { "view": "domains" } & DomainsView | { "view": "domain" } & DomainView | { "view": "volumes" } & VolumesView | { "view": "volume" } & VolumeView | { "view": "removals" } & RemovalsView | { "view": "configs" } & ConfigsView | { "view": "config" } & ConfigItemView | { "view": "branch" } & BranchView | { "view": "branch_plan" } & BranchPlanView | { "view": "build_order" } & BuildOrderView | { "view": "sync" } & SyncView | { "view": "environments" } & EnvironmentsView | { "view": "projects" } & ProjectsView | { "view": "pr_plans" } & PrPlansView | { "view": "pull_request" } & PullRequestView;

export type ConfigWritten = { "written": "project" } & ProjectCreated | { "written": "project_renamed" } & ProjectSummary | { "written": "environment" } & EnvironmentCreated | { "written": "service" } & ServiceStaged | { "written": "service_renamed" } & ServiceStaged | { "written": "service_removed" } & ServiceStaged | { "written": "volume" } & VolumeStaged | { "written": "volume_removed" } & VolumeStaged | { "written": "volume_renamed" } & VolumeStaged | { "written": "config" } & ConfigStaged | { "written": "config_removed" } & ConfigStaged | { "written": "config_renamed" } & ConfigStaged | { "written": "edited" } & Edited | { "written": "published" } & Published | { "written": "discarded" } & Discarded | { "written": "deployment" } & DeploymentSummary | { "written": "domain" } & DomainStaged | { "written": "automated" } & Automated | { "written": "branch" } & Branched | { "written": "build_order" } & BuildOrderView | { "written": "synced" } & Synced | { "written": "undone" } & Undone | { "written": "taken" } & Taken | { "written": "secret_held" } & SecretHeld | { "written": "never_synced" } & NeverSynced | { "written": "default_environment" } & EnvironmentsView | { "written": "branch_setup" } & EnvironmentsView | { "written": "environment_removed" } & Teardown<EnvironmentRemoved> | { "written": "project_removed" } & Teardown<ProjectRemoved> | { "written": "pr_plans" } & PrPlansView | { "written": "batch" } & Batched;

export type ConfigsQuery = {
/**
 * The Environment to list.
 */
environment: EnvironmentRef, };

export type ConfigsView = {
/**
 * The Environment, at the revision read.
 */
environment: EnvironmentSummary,
/**
 * Its Configs, by name.
 */
configs: Array<ConfigListing>, };

export type ConfiguredHealthcheck = { test: HealthcheckCommand, interval_millis: number | null, timeout_millis: number | null, start_period_millis: number | null, start_interval_millis: number | null, retries: number | null,
/**
 * How long a deploy waits for the first healthy result, overriding the timing-derived wait.
 */
deadline_millis?: number | null, };

export type ContainerAddress = string;

export type ContainerHostname = string;

export type ContainerId = string & { readonly __brand: "ContainerId" };

export type ContainerKind = "service_container" | "pre_deploy_hook";

export type ContainerLabels = { [key in string]: string };

export type ContainerLogRecord = { source: LogMetadata, timestamp_nanos: string, channel: LogChannel, message: string, };

export type ContainerObservation = { container_id: ContainerId,
/**
 * Generated Docker name for display, never identity or selection.
 */
display_name: string,
/**
 * Docker creation time, used only to select the newest observed Service spec.
 */
created_at_unix_nanos: number, machine_id: MachineId, namespace: Namespace, kind: ContainerKind, runtime: ContainerRuntimeObservation,
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

export type CopyNode = {
/**
 * The Branch.
 */
environment: EnvironmentRef,
/**
 * The Live Node, by name.
 */
node: ServiceName,
/**
 * Refuse with `conflict` unless Working State is still at this revision.
 */
expect: Revision | null, };

export type CopyObservation = { copies: Array<ObservedCopy>, unanswered: Array<MachineId>, };

export type CopyRole = "writer" | "slot" | "switching";

export type CpuNanos = number;

export type CreateBranch = {
/**
 * The new Branch's ID.
 */
id: EnvironmentId,
/**
 * Its Parent; the Branch joins the Parent's Project.
 */
from: EnvironmentRef,
/**
 * Its name, unique in the Project.
 */
name: EnvironmentName,
/**
 * The Parent's Services and Volumes to copy, by name. With `fix` and none
 * named, the Services the failed Deployment didn't apply.
 */
copy: Array<NodeName>,
/**
 * Nodes the Branch must use live, by name: refused unless the plan agrees.
 */
live: Array<NodeName>,
/**
 * Commands to run in an Own Copy before it first deploys, such as seeding the
 * copy of a database.
 */
setup: Array<SetupCommand>,
/**
 * Keep it after it syncs into its Parent, and never close it for being idle.
 */
keep: boolean,
/**
 * Fix this failed Deployment of the Parent on the Branch: each copied Service
 * it didn't apply starts from that Deployment's configuration, staged over
 * what the Parent runs.
 */
fix?: DeploymentId | null, };

export type CreateConfig = {
/**
 * The new Config's ID, also its lineage.
 */
id: ConfigId,
/**
 * The Environment to create it in.
 */
environment: EnvironmentRef,
/**
 * Its name, unique among the Environment's Configs.
 */
name: ConfigName,
/**
 * Where Services mount it.
 */
mounts: Array<ConfigMountAt>, };

export type CreateEnvironment = {
/**
 * The new Environment's ID.
 */
id: EnvironmentId,
/**
 * The Project to create it in; omitted means the Organization's only Project.
 */
project: ProjectName | null,
/**
 * Its name, unique in the Project.
 */
name: EnvironmentName, };

export type CreateGitService = {
/**
 * The new Service's ID, also its lineage.
 */
id: ServiceLineageId,
/**
 * The Environment to create it in.
 */
environment: EnvironmentRef,
/**
 * Its name, unique in the Environment.
 */
name: ServiceName,
/**
 * The GitHub repository, as `owner/name`.
 */
repository: RepositoryName,
/**
 * The branch to build; the repository's default branch when omitted.
 */
branch: BranchName | null, };

export type CreateProject = {
/**
 * The new Project's ID.
 */
id: ProjectId,
/**
 * Its name, unique in the Organization.
 */
name: ProjectName,
/**
 * The ID of its Default Environment, created with it.
 */
default_environment: EnvironmentId, };

export type CreateService = {
/**
 * The new Service's ID, also its lineage.
 */
id: ServiceLineageId,
/**
 * The Environment to create it in.
 */
environment: EnvironmentRef,
/**
 * Its name, unique in the Environment.
 */
name: ServiceName,
/**
 * The container image it runs; none creates an empty Service.
 */
image?: string | null,
/**
 * The Service Template it is created from, if any.
 */
template?: ServiceTemplate, };

export type CreateVolume = {
/**
 * The new Volume's ID, also its lineage.
 */
id: VolumeId,
/**
 * The Environment to create it in.
 */
environment: EnvironmentRef,
/**
 * Its name, unique among the Environment's Volumes.
 */
name: VolumeName,
/**
 * Managed storage by default; Docker storage is an explicit opt-out.
 */
storage: VolumeKind,
/**
 * Where Services mount it.
 */
mounts: Array<Mount>,
/**
 * Let more than one container write it; off refuses a second writer.
 */
shared_writes?: boolean, };

export type Cycle = "open" | "closed";

export type DataEffect = "deleted" | "kept";

export type DataLoss = { "kind": "docker_volume", id: DockerVolumeId, };

export type DataLossConfirmation = { confirmed: Array<DataLoss>, };

export type DeclareMirrorRequest = { switch: Switch, name: DockerVolumeName, refquota_bytes: number, };

export type DeleteConfig = {
/**
 * The Environment it is in.
 */
environment: EnvironmentRef,
/**
 * Its name.
 */
config: ConfigName, };

export type DependencyCondition = "service_started" | "service_healthy";

export type DependencyHealthFailure = { "type": "cancelled" } | { "type": "no_containers" } | { "type": "observation", error: RpcError, } | { "type": "container", container_id: ContainerId, failure: HealthFailure, };

export type Deploy = { id: DeploymentId, environment: EnvironmentRef,
/**
 * Deploy only these Services; none deploys every Service.
 */
services?: Array<ServiceName>,
/**
 * Refuse with `conflict` unless this is still the latest `diff` version, or the
 * version a refusal to delete data handed back.
 */
version?: string | null,
/**
 * A new upload for Services without a source of their own; none keeps the
 * Environment's latest.
 */
upload?: UploadedSource | null,
/**
 * Deployed Volumes whose data this Deploy may delete, by name. A Deploy that
 * deletes data, or publishes a removal that will, refuses with
 * `confirmation_required` unless it names each one and passes the `version`
 * that refusal handed back.
 */
accept_volume_loss?: Array<VolumeName>,
/**
 * What this Deploy ships, in the admitter's words; shown on the Deployment.
 */
message?: string | null, };

export type DeployEvent = { "type": "progress", completed: number, total: number, rows: Array<OperationRow>, } | { "type": "outcome", outcome: DeployOutcome<ExecutionError>, } | { "type": "images_pruned", report: ImageCleanupReport, };

export type DeployIntent = {
/**
 * Namespace that will own Containers this Deploy creates.
 */
namespace: Namespace,
/**
 * Complete desired Services for this Cluster.
 */
target: Array<RequestedServiceSpec>,
/**
 * Planner knobs for this Deploy, including the selected Service list.
 */
options: PlanOptions, dependencies: { [key in ServiceName]: Array<ServiceDependency> },
/**
 * Credentials each Service's private image is pulled with. They reach only the
 * Machine creating that Service's containers, never a Deploy Preview.
 */
registry_auth?: { [key in ServiceName]: RegistryAuth },
/**
 * The authored name of each Volume whose Docker name does not carry it, such as a
 * Cloud Volume named by its ID. Messages name a Volume by it.
 */
volume_names?: { [key in DockerVolumeName]: string }, };

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
 * Namespace this preview describes.
 */
namespace: Namespace,
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
 * Visible Services in the Namespace that the Deploy Intent no longer declares.
 */
would_remove: Array<QualifiedService>,
/**
 * Docker Volumes owned by this Namespace that this Deploy Intent no longer
 * declares. They are not deleted.
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
remaining_bytes: number, } | { "type": "unbudgeted_disk_usage" } | { "type": "observation_failed", gap?: ObservationGap, kind: ObservationKind, machine_id: MachineId, message: string, } | { "type": "observation_omitted", gap?: ObservationGap, kind: ObservationKind, machine_id: MachineId, } | { "type": "storage_observation_unknown",
/**
 * Machine whose storage capability could not be checked.
 */
machine_id: MachineId, } | { "type": "ingress_hostname", message: string, } | { "type": "observer_relative_hostname_conflict" } | { "type": "skipped_dependency_health", dependent: QualifiedService, dependency: QualifiedService, };

export type DeploymentId = string;

export type DeploymentQuery = { id: DeploymentId, };

export type DeploymentStatus = "queued" | "superseded" | "running" | "applied" | "failed" | "unknown" | "cancelling" | "cancelled";

export type DeploymentSummary = { id: DeploymentId,
/**
 * The Environment it deploys.
 */
environment_id: EnvironmentId,
/**
 * Counts from 1 within its Environment.
 */
number: number, status: DeploymentStatus,
/**
 * The Saved revision it ships.
 */
saved: Revision,
/**
 * The Services it targets; empty targets every Service.
 */
services: Array<ServiceName>,
/**
 * The runner that claimed it.
 */
runner: RunnerId | null,
/**
 * What its Services without a source of their own build from.
 */
upload: UploadedSource | null,
/**
 * Whether it removes the Environment from the Servers: it ships the empty
 * Environment ([`NOTHING`]) and deletes the data it accepted.
 */
remove: boolean,
/**
 * Who admitted it, as Cloud authenticated them; none when the Store's own
 * automation did, or the hidden local Store.
 */
admitted_by: Principal | null,
/**
 * When it was admitted, in Unix seconds.
 */
admitted_at: number,
/**
 * When its runner claimed it, in Unix seconds.
 */
started_at: number | null,
/**
 * When it ended, in Unix seconds: its outcome recorded, or cancelled before
 * it ran.
 */
ended_at: number | null,
/**
 * What whoever admitted it said it ships.
 */
message: string | null,
/**
 * Whether it may still run: queued, or claimed by a runner still there.
 */
in_flight: boolean,
/**
 * What its runner recorded at its end; none while it hasn't ended.
 */
outcome: Outcome | null,
/**
 * What its runner was warned of when it claimed it.
 */
warnings?: Array<DeploymentWarning>, };

export type DeploymentView = { environment: EnvironmentSummary,
/**
 * The runtime Namespace it deploys into.
 */
namespace: Namespace,
/**
 * Every node it targets.
 */
nodes: Array<NodeOutcome>,
/**
 * The Deploy Preview its runner prepared, with environment values removed.
 */
preview: DeployPreview | null,
/**
 * Its Git Services' builds, once their commits are pinned.
 */
builds: Array<BuildView>,
/**
 * Each Service's runtime name (its Private DNS name) by its name when admitted,
 * for finding its containers.
 */
runtime_names: { [key in ServiceName]: ServiceName }, id: DeploymentId,
/**
 * The Environment it deploys.
 */
environment_id: EnvironmentId,
/**
 * Counts from 1 within its Environment.
 */
number: number, status: DeploymentStatus,
/**
 * The Saved revision it ships.
 */
saved: Revision,
/**
 * The Services it targets; empty targets every Service.
 */
services: Array<ServiceName>,
/**
 * The runner that claimed it.
 */
runner: RunnerId | null,
/**
 * What its Services without a source of their own build from.
 */
upload: UploadedSource | null,
/**
 * Whether it removes the Environment from the Servers: it ships the empty
 * Environment ([`NOTHING`]) and deletes the data it accepted.
 */
remove: boolean,
/**
 * Who admitted it, as Cloud authenticated them; none when the Store's own
 * automation did, or the hidden local Store.
 */
admitted_by: Principal | null,
/**
 * When it was admitted, in Unix seconds.
 */
admitted_at: number,
/**
 * When its runner claimed it, in Unix seconds.
 */
started_at: number | null,
/**
 * When it ended, in Unix seconds: its outcome recorded, or cancelled before
 * it ran.
 */
ended_at: number | null,
/**
 * What whoever admitted it said it ships.
 */
message: string | null,
/**
 * Whether it may still run: queued, or claimed by a runner still there.
 */
in_flight: boolean,
/**
 * What its runner recorded at its end; none while it hasn't ended.
 */
outcome: Outcome | null,
/**
 * What its runner was warned of when it claimed it.
 */
warnings?: Array<DeploymentWarning>, };

export type DeploymentWarning = { config: ConfigName, file: ConfigFileName,
/**
 * The variable as the file references it, such as `api.TOKEN`.
 */
variable: string, };

export type DeploymentsQuery = { environment: EnvironmentRef,
/**
 * At most this many, 1-100 [default: 20].
 */
limit: number | null,
/**
 * The `next_cursor` of the previous page.
 */
cursor: string | null, };

export type DeploymentsView = { environment: EnvironmentSummary, deployments: Array<DeploymentSummary>,
/**
 * Pass as `cursor` for the next page; none on the last.
 */
next_cursor: string | null, };

export type Destination = {
/**
 * The Environment.
 */
name: EnvironmentName,
/**
 * The changes a Sync there would hold: the Sync view's ticked rows.
 */
changes: number,
/**
 * Its Conditional Sync there, if any.
 */
conditional_sync: DestinationSync | null, };

export type DestinationSync = { id: ConditionalSyncId,
/**
 * False once the PR Environment or the target branch changed since: sync again.
 */
standing: boolean,
/**
 * How many changes it holds.
 */
changes: number,
/**
 * The secrets it brings by name only that the Destination has no value of, as
 * `SERVICE.env.KEY`: the check waits for one, its own or held for the merge.
 */
waiting: Array<string>, };

export type DestructiveEffect = {
/**
 * What it destroys.
 */
kind: DestructiveKind,
/**
 * What a human calls the thing destroyed: the Service, the Volume, or the
 * domain's hostname.
 */
name?: string,
/**
 * The node's ID: a Service's, or a Volume's resource ID.
 */
node: string,
/**
 * The row it changes, as `diff` shows it.
 */
path: string, };

export type DestructiveKind = "removes_service" | "deletes_volume" | "detaches_volume" | "removes_domain" | "removes_server";

export type DetachConfig = {
/**
 * The Environment they are in.
 */
environment: EnvironmentRef,
/**
 * The Service, by name.
 */
service: ServiceName,
/**
 * The Config, by name.
 */
config: ConfigName, };

export type DeviceMapping = { machine_path: MachinePath, container_path: ContainerPath, cgroup_permissions: string, };

export type DeviceReservation = { driver: string | null, count: number | null, device_ids: Array<string>, capabilities: Array<Array<string>>, options: { [key in string]: string }, };

export type DiffQuery = {
/**
 * The Environment to review.
 */
environment: EnvironmentRef, };

export type DiffView = {
/**
 * The Environment reviewed.
 */
environment: EnvironmentSummary,
/**
 * Pass back to `publish` or `discard` to act on exactly this review.
 */
version: string,
/**
 * The latest Saved revision, if anything was ever published.
 */
saved: Revision | null,
/**
 * Whether Saved State already holds this Working State.
 */
published: boolean,
/**
 * Every changed node.
 */
changes: Array<NodeChange>,
/**
 * How many changes there are, counting each node and each Setting.
 */
total_count: number,
/**
 * Merged pull requests' values landed beside this Environment's own changes,
 * until its next Saved revision.
 */
hints: Array<PullRequestHint>,
/**
 * Staged changes that arrived from the Parent's deploy by Follow, still at the
 * value they arrived with, until they deploy.
 */
incoming: Array<IncomingChange>,
/**
 * The Parent's deployed values that followed into this Branch beside its own
 * changes, or that it discarded: take one to stage it.
 */
follow_hints: Array<FollowHint>,
/**
 * What deploying Working State destroys of Applied State and of what the Deployment
 * in flight puts in place, sorted; empty when nothing.
 */
effects?: Array<DestructiveEffect>, };

export type Discard = {
/**
 * The Environment to discard in.
 */
environment: EnvironmentRef,
/**
 * `SERVICE`, `volumes.VOLUME`, `configs.CONFIG`, `SERVICE.SETTING`,
 * `SERVICE.env.KEY`, `SERVICE.mounts.VOLUME` or `SERVICE.configs.CONFIG`; none
 * discards everything.
 */
path: SettingPath | null,
/**
 * Refuse with `conflict` unless this is still the latest `diff` version.
 */
version: string | null, };

export type Discarded = {
/**
 * The Environment.
 */
environment: EnvironmentSummary,
/**
 * The latest Saved revision, which follows the discard so the next Deploy ships it.
 */
saved: Revision | null,
/**
 * False when nothing it names was staged.
 */
changed: boolean, };

export type DnsLookup = { hostname: Hostname, cname: string | null, addresses: Array<string>, };

export type DnsRecord = { type: DnsRecordKind,
/**
 * Relative to the registrable domain; `@` is its apex.
 */
name: string, value: string, };

export type DnsRecordKind = "CNAME" | "A" | "AAAA";

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
used_bytes: number,
/**
 * The copy's role; unset when the plugin did not report one, which readers treat
 * as a writer.
 */
role: CopyRole | null, };

export type Domain = { service: ServiceName,
/**
 * The container port it reaches; none follows the container's `PORT`.
 */
port: number | null, } & ({ "kind": "generated", prefix: string, hostname: string | null, } | { "kind": "custom", hostname: Hostname, });

export type DomainAction = { "type": "deploy" } | { "type": "add_server" } | { "type": "dns", records: Array<DnsRecord>, };

export type DomainPrefix = string;

export type DomainQuery = { environment: EnvironmentRef,
/**
 * Its hostname, or a generated domain's prefix.
 */
domain: string, };

export type DomainRow = { status: DomainStatus,
/**
 * One short phrase on why, when it isn't plainly ready.
 */
reason: string | null, action: DomainAction | null, service: ServiceName,
/**
 * The container port it reaches; none follows the container's `PORT`.
 */
port: number | null, } & ({ "kind": "generated", prefix: string, hostname: string | null, } | { "kind": "custom", hostname: Hostname, });

export type DomainStaged = { environment: EnvironmentSummary, domain: Domain,
/**
 * Its Service, when this changed it; empty when it already was so.
 */
staged: Array<SettingPath>, };

export type DomainStatus = "ready" | "setting_up" | "needs_attention";

export type DomainView = { environment: EnvironmentSummary, domain: DomainRow, };

export type DomainsQuery = { environment: EnvironmentRef,
/**
 * Only this Service's.
 */
service: ServiceName | null, };

export type DomainsView = { environment: EnvironmentSummary, domains: Array<DomainRow>, };

export type DrainReport = {
/**
 * The drained Server's record as selected, before this Drain changed its role.
 */
server: Machine,
/**
 * Whether this Drain turned the services role off or found it off.
 */
services_role: ServicesRole,
/**
 * Each chosen Service and what the Drain did with it.
 */
services: Array<ServiceDrain>,
/**
 * Why the Drain ended before handling every Service.
 */
stopped: DrainStop | null,
/**
 * What still runs on the Server after the Drain.
 */
remaining: Remaining,
/**
 * Every chosen Service left the Server, the Drain ran to its end, and what remains
 * was observed.
 */
complete: boolean, };

export type DrainScope = { "scope": "every_namespace" } | { "scope": "owned", namespaces: Array<Namespace>, } | { "scope": "services", services: Array<QualifiedService>, };

export type DrainStop = { "kind": "cancelled" } | { "kind": "entry_unreachable", detail: string, };

export type Edit = {
/**
 * The Environment to edit.
 */
environment: EnvironmentRef,
/**
 * Refuse with `conflict` unless Working State is still at this revision.
 */
expect: Revision | null,
/**
 * The changes, applied in order.
 */
changes: Array<Change>, };

export type Edited = {
/**
 * The Environment, at its revision after the edit.
 */
environment: EnvironmentSummary,
/**
 * Settings changed in Working State, waiting for a Deploy.
 */
staged: Array<SettingPath>,
/**
 * Settings that took effect at once.
 */
immediate: Array<SettingPath>,
/**
 * Variables this edit set to a value with Typed Addresses, each with what to set
 * instead.
 */
typed_addresses: Array<TypedAddresses>, };

export type EncryptedSecretValue = { version: 1, iv: string, tag: string, ciphertext: string, };

export type EndBuildGrantRequest = { id: BuildGrantId, };

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

export type EnvironmentCreated = {
/**
 * The Environment.
 */
environment: EnvironmentSummary, };

export type EnvironmentId = string;

export type EnvironmentListing = { id: EnvironmentId, name: EnvironmentName,
/**
 * Whether it is the Project's Default Environment.
 */
default: boolean,
/**
 * The Environment it is a Branch of, if it is one.
 */
parent: EnvironmentName | null,
/**
 * Its latest Deployment, when that removes it from the Servers.
 */
removal: DeploymentSummary | null,
/**
 * What a new Branch of it runs when it names no Setup Commands.
 */
branch_setup: Array<SetupCommand>, };

export type EnvironmentName = string;

export type EnvironmentNodeType = "service" | "volume" | "config";

export type EnvironmentQuery = {
/**
 * The Environment to read.
 */
environment: EnvironmentRef,
/**
 * Narrow to one Service or one Setting; omitted means every Setting.
 */
path: SettingPath | null,
/**
 * Include Settings at their default across the whole Environment.
 */
all: boolean, };

export type EnvironmentRef = {
/**
 * The Project, by name.
 */
project: ProjectName | null,
/**
 * The Environment, by name within the Project.
 */
environment: EnvironmentName | null, };

export type EnvironmentRemoved = { environment: EnvironmentSummary, };

export type EnvironmentSummary = {
/**
 * Its durable identity.
 */
id: EnvironmentId,
/**
 * The Project it belongs to.
 */
project: ProjectName,
/**
 * Its name within the Project.
 */
name: EnvironmentName,
/**
 * Working State's revision after the request.
 */
revision: Revision, };

export type EnvironmentView = {
/**
 * The Environment, at the revision read.
 */
environment: EnvironmentSummary,
/**
 * Every Setting asked for, by Service name then Setting.
 */
settings: Array<SettingRow>,
/**
 * For one Service: its Settings as one object, the shape `set --patch` takes.
 * Settings without a value are left out.
 */
values?: { [key in string]: JsonValue } | null,
/**
 * Every row the Environment marks Never sync, whichever Settings were asked for.
 */
never_synced?: Array<RowId>, };

export type EnvironmentsQuery = {
/**
 * The Project; omitted means the Organization's only Project.
 */
project: ProjectName | null, };

export type EnvironmentsView = { project: ProjectSummary, environments: Array<EnvironmentListing>, };

export type ExecutionError = { "type": "machine", action: MachineAction, error: RpcError, } | { "type": "health", container_id: ContainerId, failure: HealthFailure, } | { "type": "dependency_health", dependency: QualifiedService, failure: DependencyHealthFailure, } | { "type": "hook", container_id: ContainerId, failure: HookFailure, } | { "type": "cancelled" };

export type ExtraHost = string;

export type FailedOperation<E> = { "type": "operation", operation: DeployOperation, error: E, } | { "type": "replacement", operation: ReplacementOperation, error: E, compensation: ReplacementCompensation<E>, };

export type FenceDecision = "admit" | "replay" | "adopt" | "refuse_stale_lease" | "refuse_stale_step";

export type FileMode = string;

export type FollowHint = {
/**
 * The Parent it comes from: pass to [`Take::from`] to use it.
 */
from: EnvironmentName,
/**
 * The Parent's value; secrets read `{"secret": true}`.
 */
value: JsonValue,
/**
 * What commands name it by; stable across renames.
 */
row: RowId,
/**
 * Its Service, Volume or Config.
 */
node: NodeName,
/**
 * Whether `node` is a Service, a Volume or a Config.
 */
kind: EnvironmentNodeType,
/**
 * Where in the node: `image`, `env.KEY`, `mounts.VOLUME`, `files.PATH`, `name`; none for the
 * node itself.
 */
name: string | null, };

export type GitSource = {
/**
 * Its runtime Service name.
 */
service: ServiceName,
/**
 * The GitHub repository, as `owner/name`.
 */
repository: RepositoryName,
/**
 * GitHub's ID for it.
 */
repository_id: RepositoryId,
/**
 * How Cloud reads it.
 */
access: ServiceGitAccess,
/**
 * The branch it follows; none once it was disconnected.
 */
branch: BranchName | null,
/**
 * The directory it builds from, inside the repository.
 */
root_dir: string,
/**
 * Its Dockerfile, relative to `root_dir`, when it builds from one.
 */
dockerfile_path: string | null,
/**
 * The commit it builds; none until pinned.
 */
commit: CommitSha | null,
/**
 * The Builders its build tries, in turn: its Preferred Builder, then the
 * Organization's Build Order.
 */
builders: Array<Builder>,
/**
 * The Server the servers try first, when it prefers one.
 */
preferred_machine: MachineId | null,
/**
 * Where its build is; none until pinned.
 */
status: BuildStatus | null,
/**
 * Why its build failed, or why the last Builder skipped it.
 */
message: string | null, };

export type GithubBuild = { id: GithubBuildId,
/**
 * The Organization whose Servers receive its image.
 */
organization: OrganizationId, status: BuildStatus, run: GithubRun,
/**
 * Set once the run checked in.
 */
grant: GithubGrant | null,
/**
 * When it checked in, in Unix seconds.
 */
checked_in_at: number | null,
/**
 * How it ended, once its final report came.
 */
ended: RunEnd | null, };

export type GithubBuildId = string;

export type GithubClaims = { repository_id: string, job_workflow_ref: string, run_id: string, event_name: string, };

export type GithubGrant = {
/**
 * Its handle; not secret.
 */
id: string,
/**
 * The Machine that minted it and receives the image.
 */
machine: MachineId,
/**
 * The build-input fingerprint the run builds against.
 */
fingerprint: string, };

export type GithubRun = { run_id: number, run_url: string,
/**
 * The workflow and branch its OIDC token must name.
 */
workflow_ref: string,
/**
 * The repository, as `owner/name` when dispatched.
 */
repository: RepositoryName, installation_id: number, };

export type GithubTimestamp = string;

export type HandOverRequest = { switch: Switch, name: DockerVolumeName, guid: SnapshotGuid, };

export type HealthFailure = { "type": "cancelled" } | { "type": "timed_out", last_check?: LastHealthCheck, } | { "type": "runtime", observation: ContainerRuntimeObservation, last_check?: LastHealthCheck, };

export type HealthObservation = "not_configured" | "starting" | "healthy" | "unhealthy" | "failing" | "stopping" | string;

export type HealthcheckCommand = [string, ...string[]];

export type HealthcheckSpec = { "state": "disabled" } | { "state": "configured" } & ConfiguredHealthcheck | { "state": "http" } & HttpHealthcheck;

export type HintSource = ConditionalSyncId | EnvironmentName;

export type HistoryContainer = { container_id: ContainerId, namespace: string | null, service: string | null, deployment: string | null, replica: string, kind: HistoryContainerKind, };

export type HistoryContainerKind = "service" | "pre_deploy_hook" | "system";

export type HistoryGapReason = "not_captured" | "corrupt";

export type HistoryStream = "stdout" | "stderr";

export type HoldSecret = {
/**
 * The Destination.
 */
environment: EnvironmentRef,
/**
 * The pull request whose merge brings the secret.
 */
pull_request: PullRequestNumber,
/**
 * The secret's row.
 */
row: RowRef,
/**
 * The value, sealed at once and never shown back.
 */
value: string, };

export type HookContainer = ContainerObservation;

export type HookFailure = { "type": "cancelled", stop_error: RpcError | null, } | { "type": "timed_out", stop_error: RpcError | null, } | { "type": "exit", code: number, };

export type HostBind = { "kind": "all" } | { "kind": "address", address: string, } | { "kind": "prefix", prefix: string, };

export type Hostname = string;

export type HttpCheckError = "connection_refused" | "timed_out" | "other";

export type HttpHealthcheck = { path: string, port: number, timeout_seconds: number, };

export type HttpProtocol = "http" | "https";

export type ImageCleanupReport = { machines: Array<MachineImageCleanup>, };

export type ImageDigest = string;

export type ImageRemoval = { reference: string, outcome: ImageRemovalOutcome, };

export type ImageRemovalOutcome = { "status": "removed" } | { "status": "in_use" } | { "status": "not_found" } | { "status": "failed", message: string, } | { "status": "unrecognized" };

export type IncomingChange = {
/**
 * Where it came from.
 */
from: EnvironmentName,
/**
 * What commands name it by; stable across renames.
 */
row: RowId,
/**
 * Its Service, Volume or Config.
 */
node: NodeName,
/**
 * Whether `node` is a Service, a Volume or a Config.
 */
kind: EnvironmentNodeType,
/**
 * Where in the node: `image`, `env.KEY`, `mounts.VOLUME`, `files.PATH`, `name`; none for the
 * node itself.
 */
name: string | null, };

export type IngressHost = string;

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

export type InspectMachineUpgradeRequest = {
/**
 * Required attempt identity, or the latest local attempt when absent.
 */
attempt_id?: MachineUpgradeAttemptId | null, };

export type InspectReceiveRequest = { name: DockerVolumeName, round: number, };

export type InspectVolumeCopyRequest = { name: DockerVolumeName, };

export type Instead = { "kind": "reference", value: string, } | { "kind": "sealed" };

export type JsonValue = number | string | boolean | Array<JsonValue> | { [key in string]: JsonValue } | null;

export type KeepBranch = {
/**
 * The Branch.
 */
environment: EnvironmentRef,
/**
 * Whether it is kept.
 */
kept: boolean, };

export type Landed = "staged" | "hint";

export type LastHealthCheck = { "type": "exited", code: number, output: string, } | { "type": "http_status", status: number, } | { "type": "http_unreachable", error: HttpCheckError, };

export type Lease = number;

export type LeaseRecord = { lease: Lease, pos: Pos, cycle: Cycle, };

export type LiveLineageUse = { lineageId: string, keys: Array<string>, };

export type LiveNode = {
/**
 * Its name where it runs.
 */
name: string,
/**
 * The nearest Environment the Branch comes from that runs it; none when
 * nothing does, so what reads it deploys empty.
 */
owner: EnvironmentName | null,
/**
 * It holds its owner's real data: a Volume, or a Service mounting one.
 */
data: boolean,
/**
 * The Branch's own Services whose variables reference it, by name.
 */
used_by: Array<ServiceName>, };

export type LiveValues = { producers: Array<SavedVariableProducer>, missing: Array<MissingLiveValue>, };

export type LiveValuesInput = { owner: LiveValuesOwner, lineages: Array<LiveLineageUse>, };

export type LiveValuesOwner = { namespace: string, producers: Array<SavedVariableProducer>, };

export type LocalMachinePhase = "uninitialized" | "joining" | "participating" | "resetting" | string;

export type LocalMachineRemoved = { reset_warning: string | null, };

export type LogChannel = "stdout" | "stderr" | "error";

export type LogHistoryRecord = { "row": "container" } & HistoryContainer | { "row": "line", container_id: ContainerId, timestamp_nanos: string, stream: HistoryStream, level: LogLevel, message: string, } | { "row": "gap", container_id: ContainerId, from_nanos: string, to_nanos: string, reason: HistoryGapReason, } | { "row": "exit", container_id: ContainerId, timestamp_nanos: string, exit_code: number | null, oom_killed: boolean, } | { "row": "end", next: string | null, };

export type LogLevel = "error" | "warn" | "info" | "debug";

export type LogMetadata = { origin: LogOrigin, machine_id: MachineId, machine_name: MachineName, };

export type LogOrigin = { "origin": "service", service_id: ServiceId, service_name: ServiceName, container_id: ContainerId, hook: string | null, } | { "origin": "machine", service: MachineLogService, };

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
accepts_ingress: boolean, id: MachineId, name: MachineName, subnet: MachineSubnet, public_key: WireGuardPublicKey, public_ip: string | null, advertised_endpoints: Array<AdvertisedEndpoint>, runtime: MachineRuntime,
/**
 * Builds this Machine runs at once; absent means automatic.
 */
build_concurrency: BuildConcurrency | null, };

export type MachineAction = "PrepareVolumes" | "CreateContainer" | "StartContainer" | "InspectContainer" | "StopContainer" | "RemoveContainer" | "RemoveVolume";

export type MachineCleanupResult = { "status": "cleaned", removals: Array<ImageRemoval>, } | { "status": "unsupported" } | { "status": "unknown", message: string, };

export type MachineDetails = { id: MachineId, phase: LocalMachinePhase, machine: Machine | null, public_key: WireGuardPublicKey, advertised_endpoints: Array<AdvertisedEndpoint>, store_version: { [key in string]: number }, rtts: Array<RttObservation>,
/**
 * Labels of Management Client slots holding an accepted or pending key.
 */
management_clients: Array<ManagementClientLabel>,
/**
 * Fresh telemetry requested only by targeted inspect.
 */
telemetry: TelemetryObservation | null,
/**
 * Current local storage evidence when the daemon advertises support.
 */
storage: MachineStorageObservation | null, };

export type MachineFailure<E> = { machine_id: MachineId, error: E, };

export type MachineId = string & { readonly __brand: "MachineId" };

export type MachineIdentity = { id: MachineId, name: MachineName, };

export type MachineImageCleanup = { machine_id: MachineId, result: MachineCleanupResult, };

export type MachineLabelKey = string;

export type MachineLabelValue = string;

export type MachineLogService = "ployz" | "docker" | "corrosion";

export type MachineName = string;

export type MachineObservation = { machine: Machine, membership: MembershipObservation,
/**
 * Raw admin evidence, absent for synthetic membership and older responders.
 */
membership_evidence?: MembershipEvidence,
/**
 * Current storage evidence, absent when this observer could not obtain it.
 */
storage: MachineStorageObservation | null, selected_endpoint: SelectedEndpoint | null,
/**
 * Entry-local RTT. `ListMachines` omits it; Runtime Watch may include it.
 */
rtt: RttStatistics | null, };

export type MachinePath = string;

export type MachineRef = { id: MachineId, name: MachineName, };

export type MachineRelease = string;

export type MachineRuntime = { daemon_version: string, docker_version: string, hostname: string, architecture: string, os_pretty_name: string, kernel_version: string,
/**
 * Host memory, absent when the daemon could not observe it.
 */
memory_total_bytes?: number | null,
/**
 * Builds this Machine's daemon is running now; live, never persisted.
 */
running_builds: number, };

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

export type MachineTelemetry = {
/**
 * Unix timestamp when this observation began.
 */
observed_at_unix_seconds: number,
/**
 * Ployz-managed Docker Container count.
 */
managed_containers: number,
/**
 * Host logical CPU count.
 */
cpu_count: number,
/**
 * One-minute host load average multiplied by 1,000.
 */
load_average_milli: number,
/**
 * Host memory total in bytes.
 */
memory_total_bytes: number,
/**
 * Host memory available in bytes.
 */
memory_available_bytes: number,
/**
 * Docker-root filesystem size in bytes.
 */
docker_root_total_bytes: number,
/**
 * Docker-root filesystem free bytes.
 */
docker_root_free_bytes: number, };

export type MachineUpdate = {
/**
 * One change per Label key: a value sets it, `None` removes it.
 */
label_changes: { [key in MachineLabelKey]: MachineLabelValue | null },
/**
 * Change Build acceptance independently; `None` preserves it and existing work remains.
 */
accepts_builds: boolean | null,
/**
 * Change application Service acceptance independently; `None` preserves it.
 */
accepts_services: boolean | null,
/**
 * Change trusted Ingress acceptance independently; `None` preserves it.
 */
accepts_ingress: boolean | null,
/**
 * Replace the Machine Name, or preserve it when omitted.
 */
name: MachineName | null,
/**
 * Explicitly preserve, remove, or replace the advertised public IP.
 */
public_ip: PublicIpUpdate,
/**
 * Replace all Advertised Endpoints, or preserve them when omitted.
 */
advertised_endpoints: Array<AdvertisedEndpoint> | null,
/**
 * Explicitly preserve, clear to automatic, or set build concurrency.
 */
build_concurrency: BuildConcurrencyUpdate, };

export type MachineUpdated = { machine: Machine, };

export type MachineUpgradeAttempt = {
/**
 * Stable identity assigned before the request is dispatched.
 */
attempt_id: MachineUpgradeAttemptId,
/**
 * Exact version resolved before any host mutation.
 */
target: MachineVersion, } & ({ "outcome": "accepted" } | { "outcome": "running",
/**
 * Latest stage durably recorded by the worker.
 */
stage: MachineUpgradeStage, } | { "outcome": "succeeded",
/**
 * Exact version reported by the ready, activated daemon.
 */
version: MachineVersion, } | { "outcome": "failed",
/**
 * First installation stage that did not complete.
 */
stage: MachineUpgradeStage,
/**
 * Operator-facing failure evidence.
 */
error: string, } | { "outcome": "interrupted",
/**
 * Last durable stage before the worker stopped.
 */
stage: MachineUpgradeStage, });

export type MachineUpgradeAttemptId = string & { readonly __brand: "MachineUpgradeAttemptId" };

export type MachineUpgradeStage = "launching" | "preparing" | "acquiring" | "verifying" | "activating" | "restarting" | "readiness" | string;

export type MachineVersion = string;

export type ManagementClientLabel = string;

export type Mark = {
/**
 * The Environment that marked it.
 */
environment: EnvironmentName,
/**
 * The marked row, as that Environment holds it.
 */
row: RowId, };

export type MembershipEvidence = { "kind": "up" } | { "kind": "suspect" } | { "kind": "down" } | { "kind": "unrecognized", raw: string, };

export type MembershipObservation = "unknown" | "up" | "suspect" | "down" | string;

export type MintBuildGrantRequest = {
/**
 * The only repository the push may write, as Docker names it (`ployz-build/web`).
 */
repository: BuildGrantRepository, };

export type MirrorMarker = { "phase": "idle" } | { "phase": "final", guid: SnapshotGuid, } | { "phase": "handed_in", guid: SnapshotGuid, } | { "phase": "promoting" };

export type MirrorRequest = { switch: Switch, name: DockerVolumeName, };

export type MissingLiveValue = { lineageId: string, key: string, };

export type Mount = {
/**
 * The Service, by name.
 */
service: ServiceName,
/**
 * The absolute path in its containers.
 */
path: string, };

export type Move = { from: MachineRef, to: MachineRef, };

export type MoveFailure = { "stage": "no_destination", from: MachineRef, detail: string, } | { "stage": "source_too_old", from: MachineRef, } | { "stage": "read_image", from: MachineRef, to: MachineRef, detail: string, } | { "stage": "copy_image", from: MachineRef, to: MachineRef, detail: string, } | { "stage": "not_serving", from: MachineRef, to: MachineRef, detail: string, replacement_removed: boolean, } | { "stage": "old_not_removed", from: MachineRef, to: MachineRef, detail: string, old_stopped: boolean, } | { "stage": "cancelled", from: MachineRef, to: MachineRef, replacement_removed: boolean, } | { "stage": "refused", reason: StayReason, };

export type NamedRow = {
/**
 * What commands name it by; stable across renames.
 */
row: RowId,
/**
 * Its Service, Volume or Config.
 */
node: NodeName,
/**
 * Whether `node` is a Service, a Volume or a Config.
 */
kind: EnvironmentNodeType,
/**
 * Where in the node: `image`, `env.KEY`, `mounts.VOLUME`, `files.PATH`, `name`; none for the
 * node itself.
 */
name: string | null, };

export type Namespace = string;

export type NamespaceQuery = { environment: EnvironmentRef, };

export type NamespaceView = { environment: EnvironmentSummary, namespace: Namespace,
/**
 * Each deployed Service's runtime name (its Private DNS name, as Applied State
 * has it), by the name it has now: a renamed Service's containers keep the name
 * it was created with, and a staged Private DNS change isn't live yet.
 */
services: { [key in ServiceName]: ServiceName }, };

export type NamespacesQuery = Record<symbol, never>;

export type NamespacesView = { namespaces: Array<OwnedNamespace>, };

export type NeverSync = {
/**
 * The Environment.
 */
environment: EnvironmentRef,
/**
 * Its rows, by name in its own configuration (with `off`, among its marks), or
 * by RowId. Each names a node the Environment has, but not necessarily a
 * variable it has yet: marking a sender's new variable keeps it out of the
 * receiver.
 */
rows: Array<RowRef>,
/**
 * Sync them again.
 */
off?: boolean, };

export type NeverSynced = {
/**
 * The Environment.
 */
environment: EnvironmentSummary,
/**
 * Every row it marks Never sync.
 */
never_synced: Array<NamedRow>, };

export type NeverSyncedRow = {
/**
 * The marks keeping it from syncing: unmark them to sync it. A new node's row
 * counts those on what it can't arrive without.
 */
marks: Array<Mark>,
/**
 * What commands name it by; stable across renames.
 */
row: RowId,
/**
 * Its Service, Volume or Config.
 */
node: NodeName,
/**
 * Whether `node` is a Service, a Volume or a Config.
 */
kind: EnvironmentNodeType,
/**
 * Where in the node: `image`, `env.KEY`, `mounts.VOLUME`, `files.PATH`, `name`; none for the
 * node itself.
 */
name: string | null, };

export type NodeChange = {
/**
 * Its name.
 */
name: string,
/**
 * Its own Sync row, which joins it to what moved it.
 */
row: RowId,
/**
 * Whether it is created, changed or removed.
 */
lifecycle: ReviewLifecycleKind,
/**
 * What `settings` compare against; `None` when nothing exists to compare.
 */
comparison: ReviewComparisonRole | null,
/**
 * Its changed Settings, by `SERVICE.SETTING` path.
 */
settings: Array<ServiceSettingChange>,
/**
 * What the change does to Volume data: `deleted` for a deployed Volume it
 * removes, `kept` for a Service that stops mounting a Volume that stays.
 */
data: DataEffect | null,
/**
 * For a Config changed in place, the deployed Services mounting it: they restart
 * to take its new files.
 */
restarts: Array<ServiceName>, type: EnvironmentNodeType, id: string, };

export type NodeName = string;

export type NodeOutcome = { outcome: NodeStatus,
/**
 * Its work on each Server, once its runner started executing.
 */
rows: Array<ServerRow>, } & ({ "type": "service", id: ServiceLineageId, name: ServiceName, } | { "type": "volume", id: VolumeId, name: VolumeName, } | { "type": "config", id: ConfigId, name: ConfigName, });

export type NodeStatus = "pending" | "deployed" | "removed" | "failed" | "not_attempted" | "unchanged" | "unknown";

export type NumberedDeploymentQuery = { environment: EnvironmentRef, number: number, };

export type ObservationGap = { machine_name: MachineName, reason: ObservationGapReason, };

export type ObservationGapReason = "failed" | "down";

export type ObservationKind = "container" | "volume";

export type ObservedCopy = { machine_id: MachineId, name: DockerVolumeName, role: CopyRole, };

export type ObservedDataLoss = { data_loss: Array<DataLoss>, };

export type OpenPullRequest = { number: PullRequestNumber,
/**
 * Empty until Cloud reports its facts.
 */
title: string, author: string, environment: EnvironmentName, };

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

export type OrganizationId = string;

export type OrganizationRemoved = { organization: OrganizationId, };

export type Outcome = { "type": "executed", summary: JsonValue, reason: string | null, cause: Array<string>, } | { "type": "not_executed", reason: string, cause: Array<string>, needs_upload: Array<ServiceName>, } | { "type": "never_ran" } | { "type": "forgotten" };

export type OwnedNamespace = { namespace: Namespace, project: ProjectName, environment: EnvironmentName, };

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

export type PlanQuery = { environment: EnvironmentRef,
/**
 * Plan only these Services; none plans every Service.
 */
services: Array<ServiceName>, };

export type PlanView = { environment: EnvironmentSummary,
/**
 * Pass to `deploy --expect-version` to ship exactly this.
 */
version: string, namespace: Namespace,
/**
 * The changes of the nodes this Deploy targets.
 */
changes: Array<NodeChange>,
/**
 * What the Deployment decides from the Servers: which containers start, stop or
 * move, and where.
 */
unresolved: Array<string>, };

export type PlannedNode = {
/**
 * Its name where the Branch comes from.
 */
name: NodeName, kind: EnvironmentNodeType,
/**
 * `own`: the Branch gets its own copy; `live`: it uses the running one;
 * `left_out`: it has none.
 */
role: PlannedRole,
/**
 * Why it is copied.
 */
because: BranchNodeReason | null,
/**
 * What it would become if the user toggled it.
 */
toggled: PlannedRole,
/**
 * Used live, the nearest Environment that runs it; none when nothing does.
 */
owner: EnvironmentName | null,
/**
 * It holds data: a Volume, or a Service mounting one.
 */
data: boolean, };

export type PlannedRole = "own" | "live" | "left_out";

export type PortPublication = { "mode": "ingress", hostname: IngressHost, load_balancer_port: number, container_port: number, http_protocol: HttpProtocol, } | { "mode": "host", bind: HostBind, published_port: number, container_port: number, transport_protocol: TransportProtocol, };

export type Pos = { seq: number, round: number, sub: number, };

export type PrEnvironment = { environment: EnvironmentSummary,
/**
 * Its latest Deployment.
 */
deployment: DeploymentSummary | null,
/**
 * Where its merge lands: each Environment that deploys the target branch.
 */
destinations: Array<Destination>, };

export type PrPlan = { repository: RepositoryName, repository_id: RepositoryId,
/**
 * The GitHub App installation its Services deploy through.
 */
installation_id: number, enabled: boolean,
/**
 * None until picked, or once that Environment is gone.
 */
start_from: EnvironmentName | null, copy: Array<NodeName>, setup: Array<SetupCommand>, remove_on_close: boolean, include_bots: boolean,
/**
 * Its pull requests with a PR Environment in the Project, not being closed.
 */
open: Array<OpenPullRequest>, };

export type PrPlansQuery = {
/**
 * The Project; omitted means the Organization's only Project.
 */
project: ProjectName | null, };

export type PrPlansView = { project: ProjectSummary, plans: Array<PrPlan>, };

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

export type Principal = string;

export type ProjectCreated = {
/**
 * The Project.
 */
project: ProjectSummary,
/**
 * Its Default Environment.
 */
environment: EnvironmentSummary, };

export type ProjectId = string;

export type ProjectListing = { id: ProjectId, name: ProjectName, default_environment: EnvironmentName,
/**
 * Its Environments, by name.
 */
environments: Array<EnvironmentName>, };

export type ProjectName = string;

export type ProjectRemoved = { project: ProjectSummary, environments: Array<EnvironmentName>, };

export type ProjectSummary = {
/**
 * Its durable identity.
 */
id: ProjectId,
/**
 * Its name.
 */
name: ProjectName, };

export type ProjectsQuery = Record<symbol, never>;

export type ProjectsView = { projects: Array<ProjectListing>, };

export type ProvisionedVolumeMaximumBytes = number;

export type PruneRefusal = "incomplete_snapshot" | "selected_services";

export type PruneTarget = { machine_id: MachineId,
/**
 * Docker's short repository name, as `docker image ls` prints it.
 */
repository: string, };

export type PublicIpUpdate = { "action": "keep" } | { "action": "remove" } | { "action": "set", "value": string };

export type Publish = {
/**
 * The Environment to publish.
 */
environment: EnvironmentRef,
/**
 * Refuse with `conflict` unless this is still the latest `diff` version, or the
 * version a refusal to delete data handed back.
 */
version: string | null,
/**
 * Deployed Volumes whose removal it may publish, by name: the next full Deploy
 * deletes their data. Publishing one refuses with `confirmation_required` unless
 * it names each one and passes the `version` that refusal handed back.
 */
accept_volume_loss?: Array<VolumeName>, };

export type PublishCertificateMaterialRequest = { hostname: CertificateHost, change: CertificateMaterialChange, };

export type Published = {
/**
 * The Environment.
 */
environment: EnvironmentSummary,
/**
 * The Saved revision that now holds Working State; none when nothing was ever
 * published and nothing is staged.
 */
saved: Revision | null,
/**
 * False when Saved State already held it, or nothing is staged.
 */
created: boolean, };

export type PublishedHostname = { hostname: Hostname,
/**
 * The Namespace of the Service publishing it.
 */
namespace: Namespace,
/**
 * The Service publishing it, by its runtime name.
 */
service: ServiceName, };

export type PullPolicy = "always" | "missing" | "never";

export type PullRequest = { repository_id: RepositoryId, number: PullRequestNumber, title: string,
/**
 * Its author's login.
 */
author: string,
/**
 * Whether its author is a bot.
 */
bot: boolean,
/**
 * The branch it merges from.
 */
head_branch: BranchName,
/**
 * That branch's head commit.
 */
head: CommitSha,
/**
 * The branch it merges into.
 */
target_branch: BranchName, commits: number, open: boolean,
/**
 * Its merge commit, once merged.
 */
merge_commit: CommitSha | null,
/**
 * Once merged: the target branch's head as the Store last saw it
 * ([`crate::ConfigStore::branch_head`]), when Cloud found the merge commit in it
 * already. Its Conditional Syncs then land with what that push deployed.
 */
merge_reached: CommitSha | null,
/**
 * When GitHub last changed it.
 */
updated: GithubTimestamp, };

export type PullRequestHint = {
/**
 * The Conditional Sync: pass to [`crate::Take::from`].
 */
conditional_sync: ConditionalSyncId,
/**
 * The pull request it merged with.
 */
pull_request: PullRequestNumber,
/**
 * The pull request's value; secrets read `{"secret": true}`.
 */
value: JsonValue,
/**
 * How it stands there.
 */
landed: Landed,
/**
 * What commands name it by; stable across renames.
 */
row: RowId,
/**
 * Its Service, Volume or Config.
 */
node: NodeName,
/**
 * Whether `node` is a Service, a Volume or a Config.
 */
kind: EnvironmentNodeType,
/**
 * Where in the node: `image`, `env.KEY`, `mounts.VOLUME`, `files.PATH`, `name`; none for the
 * node itself.
 */
name: string | null, };

export type PullRequestNumber = number;

export type PullRequestQuery = { repository_id: RepositoryId, number: PullRequestNumber, };

export type PullRequestRef = { repository_id: RepositoryId, number: PullRequestNumber, };

export type PullRequestView = {
/**
 * The latest facts Cloud reported; none before the first.
 */
pull_request: PullRequest | null,
/**
 * Its PR Environments, one per Project, not being closed.
 */
environments: Array<PrEnvironment>,
/**
 * Ready to merge: nothing waits to be synced into an Environment that deploys
 * its target branch.
 */
passing: boolean,
/**
 * Why, in a few words.
 */
reason: string, };

export type PutConfigFile = {
/**
 * The Environment it is in.
 */
environment: EnvironmentRef,
/**
 * The Config, by name.
 */
config: ConfigName,
/**
 * The file's path inside the Config.
 */
file: ConfigFileName,
/**
 * Its text, at most 256 KB.
 */
content: string,
/**
 * Its permission bits; `0444` unless given.
 */
mode?: FileMode | null,
/**
 * The user ID that owns it; `0` unless given.
 */
uid?: number | null,
/**
 * The group ID that owns it; `0` unless given.
 */
gid?: number | null, };

export type QualifiedService = string;

export type ReceiveStatus = { "state": "idle" } | { "state": "running", target: SnapshotName, } | { "state": "done", newest: Snapshot, } | { "state": "resumable", target: SnapshotName, token: string, } | { "state": "failed", target: SnapshotName, reason: string, };

export type ReceiveView = { round: number | null, status: ReceiveStatus, };

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

export type RegistryAuth = { username?: string,
/**
 * The password or access token.
 */
password: string, };

export type Remaining = { "kind": "observed",
/**
 * Every Service still with a Container there, reserved and unchosen Namespaces
 * included.
 */
services: Array<QualifiedService>,
/**
 * The user Services of `services` that the Drain's scope left alone.
 */
unchosen: Array<QualifiedService>, } | { "kind": "unobserved", error: string, };

export type Removal = { id: DeploymentId, environment: EnvironmentRef,
/**
 * As [`Deploy::version`].
 */
version?: string | null,
/**
 * As [`Deploy::accept_volume_loss`].
 */
accept_volume_loss?: Array<VolumeName>,
/**
 * Close a Branch: once this removal applied, the Store's sweep deletes it
 * without its admitter coming back. Ignored for an Environment that isn't a
 * Branch; a client that deletes it itself leaves it unset.
 */
close?: boolean, };

export type RemovalsQuery = {
/**
 * The Environment.
 */
environment: EnvironmentRef,
/**
 * Ask about the Deploy that removes the Environment from the Servers, which
 * deletes every deployed Volume.
 */
remove: boolean, };

export type RemovalsView = {
/**
 * The Environment, at the revision read.
 */
environment: EnvironmentSummary,
/**
 * The Volumes it removes; empty when a Deploy deletes no data.
 */
volumes: Array<RemovedVolume>, };

export type RemoveConfigFile = {
/**
 * The Environment it is in.
 */
environment: EnvironmentRef,
/**
 * The Config, by name.
 */
config: ConfigName,
/**
 * The file's path inside the Config.
 */
file: ConfigFileName, };

export type RemoveDomain = { environment: EnvironmentRef,
/**
 * Its hostname, or a generated domain's prefix.
 */
domain: string, };

export type RemoveEnvironment = { environment: EnvironmentRef, };

export type RemoveProject = { project: ProjectName, };

export type RemoveService = {
/**
 * The Environment it is in.
 */
environment: EnvironmentRef,
/**
 * Its name.
 */
service: ServiceName, };

export type RemoveVolume = {
/**
 * The Environment it is in.
 */
environment: EnvironmentRef,
/**
 * Its name.
 */
volume: VolumeName, };

export type RemoveVolumesRequest = { volumes: Array<DockerVolumeId>,
/**
 * Force-remove an in-use Docker Volume. Defaults to false.
 */
force: boolean, };

export type RemovedVolume = { id: VolumeId, name: VolumeName,
/**
 * The Docker Volume each Server holds its data in.
 */
docker_volume: DockerVolumeName, };

export type RenameConfig = {
/**
 * The Environment it is in.
 */
environment: EnvironmentRef,
/**
 * Its current name.
 */
config: ConfigName,
/**
 * Its new name, unique among the Environment's Configs.
 */
name: ConfigName, };

export type RenameProject = {
/**
 * The Project, by its current name.
 */
project: ProjectName,
/**
 * Its new name, unique in the Organization.
 */
name: ProjectName, };

export type RenameService = {
/**
 * The Environment it is in.
 */
environment: EnvironmentRef,
/**
 * Its current name.
 */
service: ServiceName,
/**
 * Its new name, unique in the Environment.
 */
name: ServiceName, };

export type RenameVolume = {
/**
 * The Environment it is in.
 */
environment: EnvironmentRef,
/**
 * Its current name.
 */
volume: VolumeName,
/**
 * Its new name, unique among the Environment's Volumes.
 */
name: VolumeName, };

export type ReplacementCompensation<E> = { "type": "old_untouched", stop_new_container: StopAttempt<E>, } | { "type": "old_stopped", stop_new_container: StopAttempt<E> | null, restart_old_container: RestartAttempt<E>, };

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

export type RepositoryId = number;

export type RepositoryName = string;

export type RequestMachineUpgradeRequest = {
/**
 * Identity reused by every retry of the same request.
 */
attempt_id: MachineUpgradeAttemptId,
/**
 * Exact version or supported release channel to resolve once.
 */
release: MachineRelease, };

export type RequestedServiceSpec = { name: ServiceName, mode: ServiceMode, container: ServiceContainerSpec, placement: Placement, ports: Array<PortPublication>, volumes: Array<ServiceVolume>, mounts: Array<ServiceMount>, configs: Array<ConfigSpec>, pre_deploy: PreDeployHook | null, update: UpdateConfig, };

export type ResolveVariablesInput = { parts: Array<ValuePart>, selfOwnerId: string, producers: Array<VariableProducer>, };

export type ResolveVariablesResult = { "status": "resolved", value: string, secret: boolean, warnings: Array<TemplateWarning>, } | { "status": "cycle", path: Array<string>, };

export type ResolvedServiceSpec = { service_id: ServiceId, name: ServiceName, mode: ServiceMode, container: ServiceContainerSpec, placement: Placement, ports: Array<PortPublication>, volumes: Array<ResolvedServiceVolume>, mounts: Array<ServiceMount>, configs: Array<ConfigSpec>, pre_deploy: PreDeployHook | null, update: ResolvedUpdateConfig, };

export type ResolvedServiceVolume = { reference: ServiceVolumeReference, source: ResolvedVolumeSource, };

export type ResolvedUpdateConfig = { order: UpdateOrder, monitor_millis: number | null, };

export type ResolvedVolumeSource = (Extract<VolumeSource, { kind: "ordinary" | "provisioned" }> & { scope: ScopedVolumeSource }) | (Exclude<VolumeSource, { kind: "ordinary" | "provisioned" }> & { scope: null });

export type ResolverValue = { "kind": "literal", value: string, } | { "kind": "secret", value: string, } | { "kind": "template", parts: Array<ValuePart>, };

export type RestartAttempt<E> = { "type": "restarted" } | { "type": "failed", error: E, };

export type RestartPolicy = { "name": "no" } | { "name": "always" } | { "name": "unless-stopped" } | { "name": "on-failure", maximum_retry_count: number | null, };

export type Retry = { id: DeploymentId,
/**
 * The Deployment it ships again.
 */
deployment: DeploymentId, };

export type ReviewComparisonRole = "head" | "introduction";

export type ReviewLifecycleKind = "create" | "update" | "delete";

export type ReviewNodeIdentity = { type: EnvironmentNodeType, id: string, };

export type ReviewNodeProjection = { node: ReviewNodeIdentity, config: CompiledNodeConfig | null, };

export type ReviewStateProjection = { token: string, nodes: Array<ReviewNodeProjection>, };

export type Revision = number;

export type RowId = string & { readonly __brand: "RowId" };

export type RowPhase = "starting" | "creating_container" | "starting_container" | "waiting_for_health" | "waiting_for_hook" | "stopping_container" | "removing_container" | "removing_volume" | "compensating";

export type RowRef = string;

export type RpcError = { code: RpcErrorCode, message: string, details: JsonValue,
/**
 * Each source below `message` on the peer that failed, outermost first.
 */
cause?: Array<string>, };

export type RpcErrorCode = "invalid_argument" | "not_found" | "ambiguous" | "unsupported" | "unavailable" | "conflict" | "internal" | "unauthenticated" | "confirmation_required" | string;

export type RttObservation = { peer_id: string, address: string, machine: MachineIdentity | null, statistics: RttStatistics, };

export type RttStatistics = { median_ns: number, population_stddev_ns: number, };

export type RunEnd = { "end": "built", platforms: Array<string>, } | { "end": "failed" };

export type RunnerId = string;

export type RuntimeFailureKind = "machine" | "health" | "dependency_health" | "hook" | "cancelled";

export type RuntimeOutcomeProjection = {
/**
 * Sanitized whole-attempt counts and disposition.
 */
summary: RuntimeOutcomeSummary,
/**
 * Services whose every planned operation completed, ordered by name.
 */
confirmedServices: Array<ServiceName>,
/**
 * Services work started on but didn't finish: one of their operations
 * completed or failed. Ordered by name.
 */
failedServices: Array<ServiceName>,
/**
 * Services with planned operations an earlier failure stopped before any ran.
 * Ordered by name.
 */
unattemptedServices: Array<ServiceName>, };

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

export type RuntimeWatchIncompleteIds = { machines: Array<MachineId>, containers: Array<ContainerId>, volumes: Array<DockerVolumeId>, certificates: Array<CertificateHost>, };

export type RuntimeWatchView = { services: Array<ServiceObservation>, effective_build_concurrency: { [key in MachineId]: BuildConcurrency }, machines: Array<MachineObservation>, containers: Array<ContainerObservation>, volumes: Array<DockerVolume>, certificates: Array<CertificateObservation>, incomplete_ids: RuntimeWatchIncompleteIds,
/**
 * Freshness of the entry-local membership/RTT sample. Not Cluster truth.
 */
observed_at: string, };

export type SavedConfigFile = { content: Array<ValuePart>, mode: FileMode, uid: number, gid: number, };

export type SavedConfigIntent = { resourceId: string, resourceLineageId: string, name: ConfigName, files: { [key in ConfigFileName]: SavedConfigFile }, };

export type SavedEnvironmentIntent = { version: 1, environmentSlug: string, services: Array<SavedServiceIntent>, volumes: Array<SavedVolumeIntent>,
/**
 * Written only when there are some, so documents from before Configs keep
 * their bytes and fingerprints.
 */
configs?: Array<SavedConfigIntent>, };

export type SavedServiceIntent = { id: string, lineageId: string, slug: string, config: AuthoredServiceConfig, variables: Array<SavedVariableIntent>, volumeAttachments: Array<VolumeAttachment>,
/**
 * Written only when there are some, like [`SavedEnvironmentIntent::configs`].
 */
configAttachments?: Array<ConfigAttachment>, };

export type SavedVariableIntent = { id: string, key: string, description: string | null, exported: boolean,
/**
 * Empty exactly for a [`SavedVariableValue::SecretWithoutValue`], checked by
 * `validate_variables`. It stays beside `value`, not in each valued variant:
 * moving it would reshape every stored Working State and Applied State.
 */
valueFingerprint: string, value: SavedVariableValue, };

export type SavedVariableProducer = { ownerScope: 'service', ownerId: string, ownerLineageId: string, key: string, value: SavedVariableValue, };

export type SavedVariableValue = { "kind": "literal", value: string, } | { "kind": "template", parts: Array<ValuePart>, } | { "kind": "secret",
/**
 * Absent in the browser-readable authored document. Cloud keeps the
 * ciphertext privately and captures it into each immutable publication.
 */
encryptedValue: EncryptedSecretValue | null, } | { "kind": "secret_without_value" };

export type SavedVolumeIntent = { resourceId: string, resourceLineageId: string, name: string, storage: VolumeKind,
/**
 * Whether more than one container may write it: replicas of one Service, or
 * several Services. Off, the Store refuses a second writer. Written only when on,
 * so documents from before it read as off.
 */
sharedWrites?: boolean, };

export type ScopedVolumeSource = { namespace: Namespace, logical_name: DockerVolumeName, };

export type SecretHeld = {
/**
 * The Destination.
 */
environment: EnvironmentSummary,
/**
 * The pull request it waits for.
 */
pull_request: PullRequestNumber,
/**
 * The secret's row.
 */
row: RowId, };

export type SecretRow = {
/**
 * A value is held for it to land with at the merge.
 */
held: boolean, };

export type SelectedEndpoint = string;

export type ServerRow = {
/**
 * The Machine whose work this row records, independent of its display name.
 */
machine_id: MachineId,
/**
 * The Server's name, or its Machine ID when it has none.
 */
server: string,
/**
 * When its work started, in Unix seconds.
 */
started_at: number | null,
/**
 * When it finished, in Unix seconds.
 */
finished_at: number | null, } & ({ "state": "pending" } | { "state": "running", phase: RowPhase, } | { "state": "completed" } | { "state": "failed", reason: string, cause: Array<string>, log: Array<string>, } | { "state": "not_attempted" } | { "state": "unknown" });

export type ServiceAttempt = {
/**
 * Service Name to apply from `DeployIntent.target`.
 */
name: ServiceName, };

export type ServiceBuildConfig = { buildMethod: BuildMethod, dockerfilePath: string | null,
/**
 * Override Railpack’s build command; None preserves detection. Ignored for Dockerfiles.
 */
command: string | null, };

export type ServiceConfig = { env: { [key in string]: ServiceEnvValue }, mounts: Array<ServiceDeployMount>,
/**
 * Written only when there are some, so documents from before Configs hash the same.
 */
configs?: Array<ServiceDeployConfig>, version: 2, source: ServiceSource, preDeployCommand: string | null, startCommand: string | null, healthcheck: ServiceHealthcheck, restartPolicy: ServiceRestartPolicy, maxRetries: number, replicas: number, cpuLimit: number | null, memLimit: number | null, privateDns: ServiceName, routes: Array<ServiceRoute>, managedHostnames: Array<ServiceManagedHostname>, build: ServiceBuildConfig,
/**
 * The Service Template it was created from; authoring metadata only.
 */
template?: ServiceTemplate, };

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
extra_hosts: Array<ExtraHost>, cap_add: Array<string>, cap_drop: Array<string>, healthcheck: HealthcheckSpec | null, pull_policy: PullPolicy, init: boolean | null, user: string | null, working_directory: ContainerPath | null, tty: boolean, open_stdin: boolean, privileged: boolean, pid_mode: PidMode | null, resources: ContainerResources, stop_timeout_secs: number | null, sysctls: { [key in string]: string }, restart: RestartPolicy, };

export type ServiceDependency = {
/**
 * Service that the dependent Service requires.
 */
service: ServiceName,
/**
 * Condition the dependency must satisfy before the dependent starts.
 */
condition: DependencyCondition, };

export type ServiceDeployConfig = { configResourceId: string, configName: string, mountDir: string, };

export type ServiceDeployMount = { volumeResourceId: string, volumeName: string, mountPath: string, };

export type ServiceDrain = {
/**
 * The Service, by Namespace and name.
 */
service: QualifiedService, } & ({ "result": "moved", moves: Array<Move>, } | { "result": "nothing_to_move" } | { "result": "failed", moves: Array<Move>, failure: MoveFailure, } | { "result": "stays", reason: StayReason, } | { "result": "retired" } | { "result": "not_retired", error: string, } | { "result": "interrupted", moves: Array<Move>, } | { "result": "not_attempted" });

export type ServiceEnvValue = { "kind": "literal", value: string, parts?: Array<ValuePart>, } | { "kind": "secret", variableId?: string, encryptedValue?: EncryptedSecretValue, fingerprint: string, interpolated?: boolean, };

export type ServiceGitAccess = { "type": "public" } | { "type": "github-installation", installationId: number, };

export type ServiceGitBranch = { "type": "connected", name: string, } | { "type": "disconnected", previousName: string | null, };

export type ServiceHealthcheck = { "type": "none" } | { "type": "http", path: string, timeoutSeconds: number, } | { "type": "command", command: string, timeoutSeconds: number, };

export type ServiceId = string & { readonly __brand: "ServiceId" };

export type ServiceImageCredentials = { "type": "none" } | { "type": "configured", credentialId: string, };

export type ServiceLineageId = string;

export type ServiceListing = {
/**
 * Its node's row: what a Sync or Never sync names the whole Service by.
 */
row: RowId,
/**
 * Where its image comes from.
 */
source: SourceKind,
/**
 * What the next Deploy does to it; none when it is deployed as it is.
 */
change: ReviewLifecycleKind | null,
/**
 * The Service Template it was created from, if any.
 */
template: ServiceTemplate | null,
/**
 * Its durable identity.
 */
id: ServiceLineageId,
/**
 * Its name, which Setting paths address it by.
 */
name: ServiceName,
/**
 * Its Private DNS name, fixed at creation.
 */
private_dns: ServiceName, };

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

export type ServiceQuery = {
/**
 * The Environment it is in.
 */
environment: EnvironmentRef,
/**
 * Its name.
 */
service: ServiceName, };

export type ServiceRestartPolicy = 'unless-stopped' | 'always' | 'on-failure' | 'no';

export type ServiceRoute = { id: string, hostname: string, targetPort: number | null, };

export type ServiceSettingChange = { path: string,
/**
 * The mounted Config's friendly name; its path keeps the exact identity.
 */
configName?: ConfigName, kind: ChangeKind, before: JsonValue, after: JsonValue, canRestore: boolean,
/**
 * The Sync row it falls in, which joins it to what moved it; the Store's to fill
 * from the `At` its comparison hands alongside and the node's lineage.
 */
row: RowId | null, };

export type ServiceSettingInput = { "field": "name", "value": string } | { "field": "source", "value": ServiceSource } | { "field": "rootDir", "value": string } | { "field": "command", "value": string } | { "field": "preDeployCommand", "value": string | null } | { "field": "startCommand", "value": string | null } | { "field": "healthcheck", "value": ServiceHealthcheck } | { "field": "healthcheckPath", "value": string } | { "field": "healthcheckTimeoutSeconds", "value": number } | { "field": "restartPolicy", "value": ServiceRestartPolicy } | { "field": "maxRetries", "value": number } | { "field": "replicas", "value": number } | { "field": "cpuLimit", "value": number | null } | { "field": "memLimit", "value": number | null } | { "field": "privateDns", "value": ServiceName } | { "field": "routes", "value": Array<ServiceRoute> } | { "field": "managedHostnames", "value": Array<ServiceManagedHostname> } | { "field": "managedHostnameValue", "value": ServiceManagedHostname } | { "field": "managedHostnamePrefix", "value": string } | { "field": "imageReference", "value": string } | { "field": "build", "value": ServiceBuildConfig } | { "field": "template", "value": ServiceTemplate | null };

export type ServiceSource = { "type": "empty", version: 1, rootDir: string, } | { "type": "git", version: 2, repository: string, repositoryId: number, access: ServiceGitAccess, rootDir: string, branch: ServiceGitBranch, } | { "type": "image", version: 1, image: string, credentials: ServiceImageCredentials, };

export type ServiceStaged = {
/**
 * The Service, as it is named after the change.
 */
service: ServiceSummary,
/**
 * The Environment, at its revision after the change.
 */
environment: EnvironmentSummary,
/**
 * What waits for a Deploy: every Setting of a new Service, or the Service itself
 * for a rename or removal. Empty when nothing changed.
 */
staged: Array<SettingPath>, };

export type ServiceStorageSpec = { placement: Placement, volumes: Array<ResolvedServiceVolume>, mounts: Array<ServiceMount>, };

export type ServiceSummary = {
/**
 * Its durable identity.
 */
id: ServiceLineageId,
/**
 * Its name, which Setting paths address it by.
 */
name: ServiceName,
/**
 * Its Private DNS name, fixed at creation.
 */
private_dns: ServiceName, };

export type ServiceTemplate = { id: ServiceTemplateId,
/**
 * Its version, from 1.
 */
version: number, };

export type ServiceTemplateId = string;

export type ServiceView = {
/**
 * The Environment, at the revision read.
 */
environment: EnvironmentSummary,
/**
 * The lineage its Environment copies share.
 */
lineage: ServiceLineageId,
/**
 * Its Settings as one object, the shape `set --patch` takes. A removed Service
 * shows what is deployed.
 */
values: { [key in string]: JsonValue },
/**
 * Its Settings the next Deploy changes.
 */
changes: Array<ServiceSettingChange>,
/**
 * Its node's row: what a Sync or Never sync names the whole Service by.
 */
row: RowId,
/**
 * Where its image comes from.
 */
source: SourceKind,
/**
 * What the next Deploy does to it; none when it is deployed as it is.
 */
change: ReviewLifecycleKind | null,
/**
 * The Service Template it was created from, if any.
 */
template: ServiceTemplate | null,
/**
 * Its durable identity.
 */
id: ServiceLineageId,
/**
 * Its name, which Setting paths address it by.
 */
name: ServiceName,
/**
 * Its Private DNS name, fixed at creation.
 */
private_dns: ServiceName, };

export type ServiceVolume = { reference: ServiceVolumeReference, source: VolumeSource, };

export type ServiceVolumeReference = string;

export type ServiceVolumeRequest = { switch: Switch, name: DockerVolumeName, namespace: Namespace, resolved_spec: ResolvedServiceSpec, };

export type ServicesQuery = {
/**
 * The Environment to list.
 */
environment: EnvironmentRef, };

export type ServicesRole = "turned_off" | "already_off";

export type ServicesView = {
/**
 * The Environment, at the revision read.
 */
environment: EnvironmentSummary,
/**
 * Its Services, by name.
 */
services: Array<ServiceListing>, };

export type SetBranchSetup = { environment: EnvironmentRef, setup: Array<SetupCommand>, };

export type SetBuildOrder = { build_order: BuildOrder | null, };

export type SetDefaultEnvironment = { environment: EnvironmentRef, };

export type SetGeneratedDomain = { environment: EnvironmentRef, service: ServiceName,
/**
 * Unique among the Organization's generated domains and the hostnames other
 * Namespaces publish.
 */
prefix: DomainPrefix,
/**
 * The container port it reaches: omitted keeps it, `null` follows the
 * container's `PORT`.
 */
port?: number | null, };

export type SetManagementClientResponse = { capability: string | null, };

export type SetPrPlan = {
/**
 * The Project; omitted means the Organization's only Project.
 */
project: ProjectName | null,
/**
 * The repository, like `acme/app`: one some Service of the Project deploys from.
 */
repository: RepositoryName,
/**
 * Whether its pull requests get PR Environments.
 */
enabled: boolean | null,
/**
 * The Environment each PR Environment is a Branch of.
 */
start_from: EnvironmentName | null,
/**
 * What else each copies from it, by name; the repository's Services always are.
 */
copy: Array<NodeName> | null,
/**
 * Commands to run in its Own Copies before they first deploy.
 */
setup: Array<SetupCommand> | null,
/**
 * Remove a PR Environment when its pull request closes.
 */
remove_on_close: boolean | null,
/**
 * Make PR Environments for bots' pull requests too.
 */
include_bots: boolean | null, };

export type SetVolumeSharedWrites = {
/**
 * The Environment containing the Volume.
 */
environment: EnvironmentRef,
/**
 * The Volume, by name.
 */
volume: VolumeName,
/**
 * On allows several writers; off refuses while it has more than one.
 */
shared_writes: boolean, };

export type SetVolumeStorage = {
/**
 * The Environment containing the Volume.
 */
environment: EnvironmentRef,
/**
 * The draft Volume to edit, by name.
 */
volume: VolumeName,
/**
 * Its explicit storage choice and bound.
 */
storage: VolumeKind, };

export type SettingPath = string;

export type SettingRow = {
/**
 * The Setting.
 */
path: SettingPath,
/**
 * Its value in Working State; `null` when it has none.
 */
value: JsonValue,
/**
 * The value `unset` restores.
 */
default: JsonValue,
/**
 * Whether a change to it waits for a Deploy.
 */
apply: Apply,
/**
 * A variable's row, which [`crate::NeverSync`] names it by.
 */
row?: RowId, };

export type SetupCommand = {
/**
 * The Service it runs in.
 */
service: ServiceName,
/**
 * A shell command.
 */
command: string, };

export type Skipped = { environment: EnvironmentId,
/**
 * Users read it: it holds no secret.
 */
reason: string, };

export type Snapshot = { name: SnapshotName, guid: SnapshotGuid, created_unix_seconds: number, };

export type SnapshotGuid = string;

export type SnapshotName = string;

export type SourceContainerRequest = { switch: Switch, name: DockerVolumeName, container_id: ContainerId, };

export type SourceKind = "empty" | "uploaded" | "git" | "image";

export type Start = { deployment: DeploymentId, };

export type StayReason = { "kind": "unobserved", server: MachineRef, } | { "kind": "mid_rollout" } | { "kind": "global" } | { "kind": "bind_mount", server: MachineRef, } | { "kind": "volume", server: MachineRef, } | { "kind": "no_destination", detail: string, };

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

export type Sweep = {
/**
 * Cloud's clock, in seconds since the Unix epoch.
 */
now: number, };

export type Switch = { lease: Lease, pos: Pos, };

export type SwitchError = { "reason": "stale_lease" } | { "reason": "stale_step" } | { "reason": "precondition" } | { "reason": "busy" } | { "reason": "volume_switching" } | { "reason": "no_writer" } | { "reason": "no_capacity" };

export type SwitchReply = { decision: FenceDecision,
/**
 * The record after admission.
 */
lease: LeaseRecord, copy: VolumeCopy | null, };

export type SyncChange = "new" | "changed" | "conflict";

export type SyncChanges = {
/**
 * The Environment whose changes sync.
 */
from: EnvironmentRef,
/**
 * Where they land, in the same Project; omitted, a PR Environment's only
 * Destination at the merge, else the sender's Parent.
 */
into?: EnvironmentRef | null,
/**
 * When they land; omitted, at the merge from a PR Environment into one of its
 * Destinations, else now.
 */
when?: When | null,
/**
 * Refused with `conflict` unless the Sync view is still at this version.
 */
version: string,
/**
 * The rows to sync; omitted, every row ticked. One left out is offered again
 * next time.
 */
picks?: Array<RowRef> | null,
/**
 * Rows left out of the picks, by RowId or by name in either side; a new node
 * skipped takes its rows with it.
 */
skip?: Array<RowRef>,
/**
 * A value for each picked secret the receiver lacks: sealed at once, never
 * shown back. At the merge it is held until then.
 */
values?: { [key in RowRef]: string }, };

export type SyncId = string;

export type SyncQuery = {
/**
 * As [`SyncChanges::from`].
 */
from: EnvironmentRef,
/**
 * As [`SyncChanges::into`].
 */
into?: EnvironmentRef | null,
/**
 * As [`SyncChanges::when`]; `close_after` reads nothing different.
 */
when?: When | null, };

export type SyncRow = {
/**
 * What syncing it does there.
 */
change: SyncChange,
/**
 * The value that syncs; secrets read `{"secret": true}`.
 */
from: JsonValue,
/**
 * The receiver's value now.
 */
into: JsonValue,
/**
 * Synced unless left out: every row but one the sender only inherited from a
 * Parent it isn't syncing into.
 */
ticked: boolean,
/**
 * The row of the new node it is in, which syncs with it.
 */
requires: RowId | null,
/**
 * A secret's row.
 */
secret: SecretRow | null,
/**
 * What commands name it by; stable across renames.
 */
row: RowId,
/**
 * Its Service, Volume or Config.
 */
node: NodeName,
/**
 * Whether `node` is a Service, a Volume or a Config.
 */
kind: EnvironmentNodeType,
/**
 * Where in the node: `image`, `env.KEY`, `mounts.VOLUME`, `files.PATH`, `name`; none for the
 * node itself.
 */
name: string | null, };

export type SyncView = {
/**
 * Where the changes come from.
 */
from: EnvironmentSummary,
/**
 * Where they land.
 */
into: EnvironmentSummary,
/**
 * The pull request whose merge they go live with; none when staged now.
 */
at_merge: PullRequestNumber | null,
/**
 * Pass to [`SyncChanges::version`] to sync exactly these rows.
 */
version: string,
/**
 * Each row a Sync can carry. Settings each Environment keeps as its own
 * (sizing, domains, generated addresses, the Git branch, Volume data) never are.
 */
rows: Array<SyncRow>,
/**
 * The rows it would carry but that either side marked Never sync.
 */
never_synced: Array<NeverSyncedRow>, };

export type Synced = {
/**
 * Pass to [`UndoSync::sync`] to undo it.
 */
sync: SyncId,
/**
 * Where the changes came from.
 */
from: EnvironmentSummary,
/**
 * Where they landed.
 */
into: EnvironmentSummary,
/**
 * When they land.
 */
when: SyncedWhen, };

export type SyncedWhen = { "kind": "now",
/**
 * Nodes staged in `into`'s Working State.
 */
staged: Array<NodeName>,
/**
 * The Branch is closing, as [`When::Now`] asked.
 */
closing: boolean, } | { "kind": "at_merge",
/**
 * The Conditional Sync standing now.
 */
conditional_sync: ConditionalSync, };

export type SystemEvent = { "event": "branch_head" } & BranchHead | { "event": "check_suite" } & CheckSuite | { "event": "pull_request" } & PullRequest | { "event": "sweep" } & Sweep | { "event": "cluster_forgotten" };

export type Take = {
/**
 * The retained Conditional Sync whose hints to take, or the Parent whose Follow
 * hints to take.
 */
from: HintSource,
/**
 * Its Destination, or the Branch following the Parent; refused unless it is.
 */
into?: EnvironmentRef | null,
/**
 * The hints to take; omitted, every one.
 */
rows?: Array<RowRef> | null,
/**
 * Refused with `conflict` unless the receiver's `diff` is still at this
 * version: the one the hints were read at.
 */
version: string, };

export type Taken = {
/**
 * Where the values came from: the Parent, or the pull request's PR Environment.
 */
from: EnvironmentSummary,
/**
 * Where they landed.
 */
into: EnvironmentSummary,
/**
 * Nodes staged in `into`'s Working State.
 */
staged: Array<NodeName>,
/**
 * The Conditional Sync taken from; none for a Parent's values.
 */
conditional_sync: ConditionalSync | null, };

export type Teardown<T> = { "teardown": "removed" } & T | { "teardown": "waiting", environment: EnvironmentName, deployment: DeploymentId, } | { "teardown": "needs_removal", environment: EnvironmentName, deployment: DeploymentId, };

export type TelemetryObservation = { "scope": "bridge_capacity",
/**
 * Fresh Ployz bridge endpoint capacity.
 */
bridge: BridgeEndpointCapacity, } | { "scope": "full",
/**
 * Fresh host telemetry.
 */
host: MachineTelemetry,
/**
 * Fresh Ployz bridge endpoint capacity.
 */
bridge: BridgeEndpointCapacity, };

export type TemplateWarning = { kind: 'missing', ownerId: string | null, key: string, };

export type TransportProtocol = "tcp" | "udp";

export type TypedAddresses = {
/**
 * The variable, as SERVICE.env.KEY.
 */
path: SettingPath,
/**
 * The Services whose private addresses it types.
 */
services: Array<ServiceName>,
/**
 * What to set instead.
 */
instead: Instead, };

export type Ulimit = { soft: number, hard: number, };

export type Unclaimed = { organization: OrganizationId, environment: EnvironmentId, deployment: DeploymentId,
/**
 * When it was admitted, in Unix seconds.
 */
admitted_at: number, };

export type UndoSync = {
/**
 * As [`Synced::sync`].
 */
sync: SyncId, };

export type Undone = {
/**
 * The receiver now.
 */
into: EnvironmentSummary, };

export type UpdateConfig = {
/**
 * Absence means derive the order from the deploy snapshot.
 */
order: UpdateOrder | null, monitor_millis: number | null, };

export type UpdateOrder = "start_first" | "stop_first";

export type UploadBase = { commit: CommitSha,
/**
 * Whether the directory held changes the commit doesn't.
 */
changed: boolean, };

export type UploadDigest = string;

export type UploadedSource = { digest: UploadDigest,
/**
 * The commit the directory was checked out at, if it was a Git checkout.
 * Provenance only: it never identifies the build.
 */
base: UploadBase | null,
/**
 * Who uploaded it, as Cloud authenticated them; admission overwrites whatever a
 * caller sends. Provenance only.
 */
uploader?: Principal | null, };

export type ValuePart = { "kind": "text", value: string, } | { "kind": "ref", owner: ValuePartOwner, key: string, };

export type ValuePartOwner = { "scope": "self" } | { "scope": "service", lineageId: string, };

export type VariableProducer = { ownerId: string, owner: ValuePartOwner, key: string, value: ResolverValue, };

export type VolumeAttachment = { volumeResourceId: string, mountPath: string, };

export type VolumeConfig = { version: 2, name: string, storage: VolumeKind, };

export type VolumeCopy = { "kind": "root", writer: WriterMarker, readonly: boolean, newest: Snapshot | null, } | { "kind": "slot", mirror: MirrorMarker, readonly: boolean, newest: Snapshot | null, resume_token: string | null, };

export type VolumeCopyView = { copy: VolumeCopy | null, lease: LeaseRecord | null, };

export type VolumeDriver = { name: string, options: { [key in string]: string }, };

export type VolumeId = string;

export type VolumeKind = { "kind": "docker", } | { "kind": "provisioned", maximumBytes: ProvisionedVolumeMaximumBytes, };

export type VolumeListing = {
/**
 * The Services mounting it in Working State.
 */
mounts: Array<Mount>,
/**
 * Whether a Deploy applied it, so the Servers may hold its data.
 */
deployed: boolean,
/**
 * An admitted attempt fixes the storage choice, even if it fails.
 */
storage_locked: boolean,
/**
 * What the next Deploy does to it; none when it is deployed as it is.
 */
change: ReviewLifecycleKind | null,
/**
 * Its durable identity.
 */
id: VolumeId,
/**
 * Its name, which mount paths address it by.
 */
name: VolumeName,
/**
 * Its chosen storage, including the maximum for a Provisioned Volume.
 */
storage: VolumeKind,
/**
 * Whether more than one container may write it.
 */
shared_writes: boolean, };

export type VolumeName = string;

export type VolumeObservation = {
/**
 * The Docker Volumes asked about.
 */
sought: Array<DockerVolumeName>,
/**
 * Each one found, on the Server holding it.
 */
held: Array<DockerVolumeId>,
/**
 * Servers that did not answer, or were not asked: what they hold is unknown.
 */
unanswered: Array<MachineId>, };

export type VolumeQuery = {
/**
 * The Environment it is in.
 */
environment: EnvironmentRef,
/**
 * Its name.
 */
volume: VolumeName, };

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

export type VolumeStaged = {
/**
 * The Volume.
 */
volume: VolumeSummary,
/**
 * The Environment, at its revision after the change.
 */
environment: EnvironmentSummary,
/**
 * What waits for a Deploy: the Volume as `volumes.NAME`, and each mount it
 * gained or lost as `SERVICE.mounts.NAME`.
 */
staged: Array<SettingPath>, };

export type VolumeSummary = {
/**
 * Its durable identity.
 */
id: VolumeId,
/**
 * Its name, which mount paths address it by.
 */
name: VolumeName,
/**
 * Its chosen storage, including the maximum for a Provisioned Volume.
 */
storage: VolumeKind,
/**
 * Whether more than one container may write it.
 */
shared_writes: boolean, };

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

export type VolumeView = {
/**
 * The Environment, at the revision read.
 */
environment: EnvironmentSummary,
/**
 * The lineage its Environment copies share.
 */
lineage: VolumeId,
/**
 * The Services mounting it in Working State.
 */
mounts: Array<Mount>,
/**
 * Whether a Deploy applied it, so the Servers may hold its data.
 */
deployed: boolean,
/**
 * An admitted attempt fixes the storage choice, even if it fails.
 */
storage_locked: boolean,
/**
 * What the next Deploy does to it; none when it is deployed as it is.
 */
change: ReviewLifecycleKind | null,
/**
 * Its durable identity.
 */
id: VolumeId,
/**
 * Its name, which mount paths address it by.
 */
name: VolumeName,
/**
 * Its chosen storage, including the maximum for a Provisioned Volume.
 */
storage: VolumeKind,
/**
 * Whether more than one container may write it.
 */
shared_writes: boolean, };

export type VolumesQuery = {
/**
 * The Environment to list.
 */
environment: EnvironmentRef, };

export type VolumesView = {
/**
 * The Environment, at the revision read.
 */
environment: EnvironmentSummary,
/**
 * Its Volumes, by name.
 */
volumes: Array<VolumeListing>, };

export type WarmRequest = { switch: Switch, name: DockerVolumeName, };

export type When = { "kind": "now",
/**
 * Close the Branch once its changes landed in its Parent: refused for a
 * kept Branch, and for a Sync into anything but its Parent.
 */
close_after?: boolean, } | { "kind": "at_merge" };

export type WireGuardPublicKey = Array<number>;

export type WriterMarker = { "phase": "idle" } | { "phase": "stopping" } | { "phase": "frozen", guid: SnapshotGuid, } | { "phase": "thawing" } | { "phase": "handed", guid: SnapshotGuid, };

