import type {
  ConfigCommand,
  ConfigQuery,
  ConfigTrusted,
  VolumeObservation,
  ContainerObservation,
  ConfigView,
  ConfigWritten,
  ConfigCommitted,
  Converted,
  PullRequestRef,
  OrganizationRemoved,
  AppliedVolume,
  Unclaimed,
  DeploymentSummary,
  GitSource,
  SystemEvent,
  GithubBuild,
  GithubClaims,
  GithubRun,
  BuildStatus,
  CertificateMaterialPublished,
  ContractDescription,
  DeployEvent,
  DeployIntent,
  DeployOutcome,
  DeployPreview,
  VolumeRemoval,
  ExecutionError,
  ImageCleanupReport,
  PruneTarget,
  MachineId,
  MachineDetails,
  MachineTarget,
  MachineUpdate,
  MachineUpdated,
  MachineUpgradeAttempt,
  RequestMachineUpgradeRequest,
  InspectMachineUpgradeRequest,
  ObservedDataLoss,
  LocalMachineRemoved,
  DataLossConfirmation,
  DrainReport,
  DrainScope,
  ClusterTeardown,
  PlanOptions,
  Namespace,
  PublishCertificateMaterialRequest,
  RegisterRequest,
  Registered,
  EnrollmentAssignment,
  EnrollmentSnapshot,
  RemoveVolumesRequest,
  RequestedServiceSpec,
  RpcError as RpcErrorPayload,
  RuntimeWatchView,
  ContainerLogRecord,
} from "./generated/payloads";
export * from "./generated/payloads";
import type {
  CommitRequest,
  CopyObservation,
  DeclareMirrorRequest,
  HandOverRequest,
  InspectReceiveRequest,
  InspectVolumeCopyRequest,
  MirrorRequest,
  ReceiveView,
  ServiceVolumeRequest,
  SnapshotGuid,
  SourceContainerRequest,
  SwitchReply,
  Switch,
  VolumeCopyView,
  WarmRequest,
} from "./generated/payloads";

/** StartReceive on a mirror: pull `target` from the writer Machine at management address `from`. */
export type StartReceiveRequest = {
  switch: Switch;
  name: string;
  from: string;
  base: SnapshotGuid | null;
  target: string;
  resume_token: string | null;
};

/** One request `Client.volumeSwitch` sends: a Volume run verb and its payload. */
export type VolumeSwitchRequest =
  | { command: "inspect_volume_copy"; payload: InspectVolumeCopyRequest }
  | { command: "declare_mirror"; payload: DeclareMirrorRequest }
  | { command: "begin_round"; payload: MirrorRequest }
  | { command: "commit_snapshots"; payload: CommitRequest }
  | { command: "warm_snapshot"; payload: WarmRequest }
  | { command: "start_receive"; payload: StartReceiveRequest }
  | { command: "inspect_receive"; payload: InspectReceiveRequest }
  | { command: "prune_mirror"; payload: MirrorRequest }
  | { command: "destroy_mirror"; payload: MirrorRequest }
  | { command: "forget_snapshots"; payload: MirrorRequest }
  | { command: "forget_lease"; payload: MirrorRequest }
  | { command: "withdraw"; payload: SourceContainerRequest }
  | { command: "freeze"; payload: SourceContainerRequest }
  | { command: "hand_over"; payload: HandOverRequest }
  | { command: "thaw"; payload: SourceContainerRequest }
  | { command: "close"; payload: MirrorRequest }
  | { command: "accept_hand_off"; payload: HandOverRequest }
  | { command: "promote"; payload: ServiceVolumeRequest }
  | { command: "start_handed_container"; payload: ServiceVolumeRequest }
  | { command: "clear_final"; payload: MirrorRequest };

/** The reply payload of one Volume run verb. */
export type VolumeSwitchReply<C extends VolumeSwitchRequest["command"]> =
  C extends "inspect_volume_copy" ? VolumeCopyView : C extends "inspect_receive" ? ReceiveView : SwitchReply;

/** Same serialized descriptors as CLI contexts. Backend only: Management is an admin capability. */
export type Connection = (
  | { readonly management: string }
  | { readonly ssh: string; readonly ssh_key_file?: string }
  | { readonly tcp: string }
  | { readonly unix: string }
) & { readonly machine_id?: MachineId };
export type ConnectOptions = {
  readonly connections: readonly Connection[];
  /** Cancels connection establishment and closes the resulting session. */
  readonly signal?: AbortSignal;
  /** Total connection/session lifetime budget; close cancels this timer. */
  readonly timeoutMs?: number;
};

export type WatchOptions = {
  readonly signal?: AbortSignal;
};

export type LogFilter = {
  namespace?: string; serviceId?: string; serviceName?: string; deploymentId?: string;
  machineId?: string; containerId?: string; kind?: "service_container" | "pre_deploy_hook";
};
export type LogRecord = ContainerLogRecord & { id: string };
export type LogSourceError = { type: "source_error"; machineId: string; containerId: string; message: string };
export type LogEvent = { type: "record"; record: LogRecord } | LogSourceError;
export type LogOptions = WatchOptions & { filter?: LogFilter; tail?: number; follow?: boolean };
export type LogHistoryOptions = WatchOptions & { filter?: LogFilter; before: Record<string, string>; limit?: number };
export type LogHistoryPage = { records: LogRecord[]; errors: LogSourceError[] };

export type ConfirmOptions = WatchOptions & {
  deploymentId?: string;
  /** "auto" (default) cleans up after the Outcome, before `finished` settles. "manual" leaves it to `pruneImages(pruneTargets)`. */
  imageCleanup?: "auto" | "manual";
};

export type RunOptions = WatchOptions;

export declare const RpcError: {
  new (error: RpcErrorPayload, options?: ErrorOptions): Error & RpcErrorPayload;
};

/** Private build evidence; never proof that content still exists on a Machine. */
export type BuildReceipt = {
  fingerprint: string;
  /** The variables the build read: every one, or an uploaded Dockerfile's declared `ARG`s. */
  variables: "all" | { declared: string[] };
  image: { reference: string; tags: string[]; platforms: string[]; location: string };
  machine_id: MachineId;
};
export type BuildReceipts = Record<string, BuildReceipt>;

export type PreparedDeploy = DeployPreview & {
  readonly buildReceipts: BuildReceipts;
  readonly noop: boolean;
  /** Image Cleanup scope as plain data, for a later `pruneImages`. */
  readonly pruneTargets: PruneTarget[];
  /** Release unconfirmed retained image resources. */
  close(): void;
  confirm(options?: ConfirmOptions): RunningDeploy;
};

/** Backend-only input. Checkout paths are repository roots, keyed by config.privateDns. */
export type PreparationInput = {
  deployment: {
    namespace: string;
    selected?: import("./generated/payloads").ServiceAttempt[];
    /** Service ID by lineage, from the frozen variable producers; references through it order the deploy. */
    lineages?: Record<string, string>;
    snapshots: readonly { serviceId?: string; config: import("./generated/payloads").ServiceConfig; replicas?: number; resolvedEnv?: Record<string, string>; setupCommands?: readonly string[] }[];
    volumes?: readonly { volumeResourceId: string; storage: import("./generated/payloads").VolumeKind }[];
  };
  sources: Record<string, string>;
  source_commits?: Record<string, string>;
  /** Uploaded Source content digests, keyed by config.privateDns. A `sources` directory must hold exactly that content; without one only a matching, usable receipt serves it. */
  uploads?: Record<string, string>;
  /** Earlier images to try, in order: the Service's own first, then other Environments' of the same build inputs. */
  build_receipts?: Record<string, BuildReceipt[]>;
  /** This build's position among its attempt's builds; builds without a warm Machine spread across Machines by it. */
  build_index?: number;
  /** The Service's Preferred Machine, the Cluster's first choice to build. */
  preferred_machine?: MachineId;
};

/** Why a Machine was chosen to build. `name` is null once the Machine left the Cluster. */
export type BuilderReason =
  | { kind: "preferred" }
  | { kind: "had_cache" }
  | { kind: "spread" }
  | { kind: "cache_holder_unavailable"; holder: MachineId; name: string | null }
  | { kind: "preferred_unavailable"; preferred: MachineId; name: string | null };

/** One BuildKit step; `id` is stable across repeated reports, timestamps are RFC 3339. */
export type BuildStep = { id: string; name: string; started: string | null; completed: string | null; cached: boolean; error: string | null };

export type PreparationEvent =
  | { Platforms: string[] }
  | { Selected: { machine: import("./generated/payloads").Machine; reason: BuilderReason; rejections: string[] } }
  | { Build: { Stage: string } | { Output: number[] } | { Step: BuildStep } | { StepOutput: { step: string; stderr: boolean; text: string } } | { Timing: unknown } | { Target: { name: string; outcome: unknown } } }
  | "Transfer"
  /** One Service's image starts going to these Machines, by name. */
  | { Sending: { service: string; machines: string[] } }
  /** One Machine received a Service's image. */
  | { Delivered: { image: string; service: string; machine_id: MachineId } };

export type RunningPreparation = AsyncIterable<PreparationEvent> & {
  abort(): void;
  /** Completes without consuming progress; failures preserve typed stage and work evidence. */
  readonly finished: Promise<PreparedDeploy>;
};

export type BuildOptions = WatchOptions & {
  /** Withdraw the build when no Build Machine admits it within this many ms. Omit to wait in the queue. */
  readonly startWithinMs?: number;
};

/** `queued`: not admitted within `startWithinMs`, withdrawn; nothing started. */
/** `built`: `reused` when an earlier image of the same build inputs served it and nothing was built. */
export type BuildOutcome = { kind: "queued" } | { kind: "built"; receipt: BuildReceipt; reused: boolean };

export type RunningBuild = AsyncIterable<PreparationEvent> & {
  abort(): void;
  /** Rejects like `RunningPreparation.finished`; cancellation leaves no receipt. */
  readonly finished: Promise<BuildOutcome>;
};

export type RunningDeploy = AsyncIterable<DeployEvent> & {
  abort(): void;
  /** Rejects with RpcError on session closure; an in-flight mutation may have completed. */
  readonly finished: Promise<DeployOutcome<ExecutionError>>;
};

export declare function connect(options: ConnectOptions): Promise<Client>;
/** The ployz version every fingerprint covers; a GitHub runner installs exactly this one. */
export declare function ployzVersion(): string;
export declare function applyAll(
  namespace: Namespace,
  specs: readonly RequestedServiceSpec[],
  options?: PlanOptions,
): DeployIntent;

export declare function applyOne(
  namespace: Namespace,
  spec: RequestedServiceSpec,
  options?: PlanOptions,
): DeployIntent;

export declare class Client {
  prepare(input: PreparationInput, options?: WatchOptions): RunningPreparation;
  /** One Image Build. `input` holds exactly one Git Service with its checkout and commit; its receipt is a reuse hint. */
  build(input: PreparationInput, options?: BuildOptions): RunningBuild;
  /** Sets `label`'s slot to a fresh key; resolves to its secret `ployz1:` Management Capability. */
  setManagementClient(label: string): Promise<string>;
  clearManagementClient(label: string): Promise<void>;
  inspect(): Promise<MachineDetails>;
  observeEnrollment(): Promise<EnrollmentSnapshot>;
  register(assignment: EnrollmentAssignment): Promise<Registered>;
  about(): Promise<ContractDescription>;
  /** Idempotent. Rejects with invalid_argument when the chain, key, or hostname coverage fails. */
  publishCertificateMaterial(
    request: PublishCertificateMaterialRequest,
  ): Promise<CertificateMaterialPublished>;
  readonly runtime: {
    watch(options?: WatchOptions): AsyncIterable<RuntimeWatchView>;
    logs(options?: LogOptions): AsyncIterable<LogEvent>;
    logHistory(options: LogHistoryOptions): Promise<LogHistoryPage>;
  };
  preview(intent: DeployIntent): Promise<PreparedDeploy>;
  previewNamespaceRemoval(
    namespace: Namespace,
    destroy_volumes: boolean,
  ): Promise<PreparedDeploy>;
  run(
    intent: DeployIntent,
    options?: RunOptions,
  ): Promise<DeployOutcome<ExecutionError>>;
  /** Per-Machine results; never rejects for a Machine's failure. */
  pruneImages(targets: readonly PruneTarget[]): Promise<ImageCleanupReport>;
  removeVolumes(
    request: RemoveVolumesRequest,
  ): Promise<VolumeRemoval[]>;
  dataLossIfMachineRemoved(machine: MachineTarget): Promise<ObservedDataLoss>;
  removeMachine(
    machine: MachineTarget,
    confirmDataLoss: DataLossConfirmation,
  ): Promise<LocalMachineRemoved>;
  /**
   * Turn a Server's services role off, retire its Globals there, and move each replicated
   * Service in `scope` off it, one at a time and start-first. Resolves with the report even
   * when it is partial or the session closes mid-drain; rejects only before anything moved.
   */
  drainMachine(machine: MachineTarget, scope: DrainScope): Promise<DrainReport>;
  /** Take a Machine out of the Cluster without resetting it: it keeps its state and keys. */
  removeMachineMembership(machine: MachineTarget): Promise<void>;
  /** One Machine policy edit (Machine Roles and build concurrency); omitted fields keep their values. */
  updateMachine(
    machine: MachineTarget,
    update: Partial<MachineUpdate>,
  ): Promise<MachineUpdated>;
  /** Container `container` on `machine` as its daemon holds it, its spec's environment values included. */
  inspectContainer(machine: MachineTarget, container: string): Promise<{ container: ContainerObservation }>;
  /**
   * Copy the image Container `container` runs on `source` to `dest`, by its local image ID and
   * tagged as its spec names it. Never asks a registry; a no-op when `dest` already holds it.
   */
  copyContainerImage(source: MachineTarget, container: string, dest: MachineTarget): Promise<void>;
  /**
   * Send one Volume switch request to one Machine and answer its reply payload. Only the Volume
   * run verbs are accepted; any other command rejects `invalid_argument`. Not retried. A fence
   * refusal rejects with an `RpcError` whose `details` is a `SwitchError`.
   */
  volumeSwitch<R extends VolumeSwitchRequest>(
    machine: MachineTarget,
    request: R,
  ): Promise<VolumeSwitchReply<R["command"]>>;
  /** Ask one Machine to Upgrade. Repeating the request with the same attempt ID returns that attempt. */
  requestMachineUpgrade(
    machine: MachineTarget,
    request: RequestMachineUpgradeRequest,
  ): Promise<MachineUpgradeAttempt>;
  /** Read one Machine's Upgrade attempt. Its daemon restarts during the Upgrade, so a caller polling for the outcome keeps polling through `unavailable`. */
  inspectMachineUpgrade(
    machine: MachineTarget,
    request: InspectMachineUpgradeRequest,
  ): Promise<MachineUpgradeAttempt>;
  dataLossIfNamespaceDestroyed(
    namespace: Namespace,
    destroy_volumes?: boolean,
  ): Promise<ObservedDataLoss>;
  destroyNamespace(
    namespace: Namespace,
    confirmDataLoss: DataLossConfirmation,
    destroy_volumes?: boolean,
  ): Promise<DeployOutcome<ExecutionError>>;
  dataLossIfClusterDestroyed(): Promise<ObservedDataLoss>;
  destroyCluster(confirmDataLoss: DataLossConfirmation): Promise<ClusterTeardown>;
  close(): Promise<void>;
}

export declare function allocateEnrollment(request: RegisterRequest, snapshot: EnrollmentSnapshot, saved: EnrollmentAssignment[]): EnrollmentAssignment;

/** One Config Store; every call acts in one Organization and rejects with RpcError. */
export interface ConfigStore {
  /**
   * The view of the same name as the query. `trusted` is what Cloud observed itself, such as a domain's certificates;
   * never the caller's.
   */
  read<Q extends ConfigQuery>(organization: string, query: Q, trusted?: ConfigTrusted): Promise<Extract<ConfigView, { view: Q["query"] }>>;
  /**
   * As `principal` (who Cloud authenticated; none for Cloud itself). `trusted` is evidence Cloud gathered itself, such
   * as readable repositories; never the caller's.
   */
  write(organization: string, command: ConfigCommand, trusted?: ConfigTrusted, principal?: string | null): Promise<ConfigCommitted>;
  /** What converting Conditional Syncs to offers did, when opening this Store converted them; null once an earlier open did. */
  converted(): Converted | null;
  /** Cloud only: the pull requests whose checks a change in Environment `environment` (its ID) may move. */
  checks(environment: string): Promise<PullRequestRef[]>;
  /** Cloud's worker only: the Git Services the Deployment builds, each with its pinned commit, if any. */
  deploymentSources(deployment: string): Promise<GitSource[]>;
  /**
   * Cloud's worker only: pin the commits it resolved, by runtime Service name. A pinned commit never changes; resolves
   * to every source with its pin, or rejects `conflict` once the Deployment was replaced, cancelled or ended.
   */
  pinSources(deployment: string, commits: Record<string, string>): Promise<GitSource[]>;
  /**
   * Cloud's worker only: claim the queued Deployment as `runner`, build its Git Services from `sources.checkouts`
   * (directories by runtime Service name, at their pinned commits) and its uploaded Services from `sources.upload` (the
   * directory its upload was extracted to; without it they reuse a usable image or need a new upload), deploy it on one
   * of `connections` and record its outcome. `sources.failure` says why Cloud could not read the sources: it is
   * recorded as why nothing ran. Its secrets and evidence stay in Rust; it resolves to the summary, or rejects
   * `conflict` when this runner has nothing to run.
   */
  runDeployment(
    organization: string, deployment: string, runner: string, connections: Connection[],
    sources?: { checkouts?: Record<string, string>; upload?: string; failure?: string },
  ): Promise<DeploymentSummary>;
  /** Cloud's worker only: `runner` stopped without finishing; the outcome is unknown once it prepared. */
  abandonDeployment(deployment: string, runner: string): Promise<DeploymentSummary>;
  /**
   * Cloud's GitHub workers only: apply what Cloud observed of GitHub; resolves to `{written: "automated", …}` with the
   * Deployments it admitted (`trusted.servers`: how many Servers could run them; `trusted.domains.published`: hostnames they may not take), or rejects `conflict` when a branch head's `base` is no longer the Store's head.
   */
  system(organization: string, event: SystemEvent, trusted?: Pick<ConfigTrusted, "servers" | "domains">): Promise<ConfigWritten>;
  /** Cloud's own Organization removal only: forget its configuration once it has no Project; else rejects `conflict`. */
  removeOrganization(organization: string): Promise<OrganizationRemoved>;
  /** Cloud's Forget Servers only: the Volumes a Deploy put on the Organization's Servers, which `cluster_forgotten` lets go of. */
  appliedVolumes(organization: string): Promise<AppliedVolume[]>;
  /** Cloud's sweep only: every queued Deployment no runner claimed, admitted before `before` (Unix seconds). */
  unclaimed(before: number): Promise<Unclaimed[]>;
  /** Cloud's GitHub workers only: the branch head the Store last saw, which a new head is compared from. */
  branchHead(organization: string, repositoryId: number, branch: string): Promise<string | null>;
  /**
   * Cloud's GitHub workers only: before a push reaches the Store, the pull requests into the branch that a draft
   * includes or is offered, not known to have merged (report each that did).
   */
  pendingSyncs(organization: string, repositoryId: number, branch: string): Promise<{ standing: number[] }>;
  /**
   * Cloud's worker only: start GitHub build `build` (`DEPLOYMENT.SERVICE`). An earlier image may serve it (`reused`);
   * GitHub may be unable to take it (`skipped`, recorded for the next Builder); else dispatch on `runner`.
   */
  githubStart(build: string, connections: Connection[]): Promise<GithubStart>;
  /** Cloud's worker only: hand the build to its dispatched run; rejects `conflict` when it is no longer wanted. */
  githubDispatched(build: string, run: GithubRun): Promise<null>;
  /** Cloud only: a build handed to GitHub. */
  githubBuild(build: string): Promise<GithubBuild>;
  /** Cloud's worker only: GitHub can't take the build before any run; the next Builder takes it. */
  githubSkip(build: string, message: string): Promise<BuildStatus>;
  /**
   * The check-in route only, with the OIDC claims Cloud verified: the runner's Build Grant and build inputs, secrets
   * included. Rejects `unauthenticated` for another repository, workflow or run; `conflict` when refused.
   */
  githubCheckIn(build: string, claims: GithubClaims, connections: Connection[]): Promise<GithubCheckIn>;
  /** The steps route only: take a runner's report (`{from, events, platforms?, installFailed?}`). */
  githubReport(build: string, claims: GithubClaims, report: unknown): Promise<{ received: number; ended: boolean }>;
  /** Cloud only: end a build whose run reported its end, completed or timed out: end its grant, write its receipt. */
  githubFinish(build: string, timedOut: boolean, connections: Connection[]): Promise<GithubFinish>;
  /** Cloud only: end the grants of a Deployment's builds still on GitHub and fail them; cancel the returned runs. */
  githubCancel(deployment: string, connections: Connection[]): Promise<GithubBuild[]>;
}
export type GithubStart = { kind: "reused" } | { kind: "dispatch"; runner: string } | { kind: "skipped"; message: string };
export type GithubCheckIn = { grant: string; commit: string; fingerprint: string; ployzVersion: string; deployment: PreparationInput["deployment"] };
export type GithubFinish = "waiting" | { ended: BuildStatus };
/**
 * Open the Config Store at `url` (`postgres://…`, or `sqlite:PATH` in tests), migrating it.
 * Secrets are sealed with a key derived from `sealingSecret` (Cloud's encryption secret).
 * Calls run off the JavaScript thread, a few at once; a slow or queued call rejects `unavailable`.
 */
export declare function openConfigStore(url: string, sealingSecret: string): Promise<ConfigStore>;
/**
 * Which of `connections`' Servers hold each Docker Volume in `sought`: the `volumes` evidence of `ConfigTrusted` for
 * admitting a Deploy that removes deployed Volumes. Servers that don't answer are listed as `unanswered`.
 */
export declare function observeVolumes(connections: Connection[], sought: string[]): Promise<VolumeObservation>;
/**
 * Every copy of every Volume on `connections`' Servers, by role. Servers that don't answer are listed as
 * `unanswered`, never assumed to hold nothing.
 */
export declare function observeCopies(connections: Connection[]): Promise<CopyObservation>;
