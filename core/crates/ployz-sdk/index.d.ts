import type {
  BuildGrantEnded,
  BuildGrantMinted,
  ConfigCommand,
  ConfigQuery,
  ConfigTrusted,
  VolumeObservation,
  ConfigView,
  ConfigWritten,
  OrganizationRemoved,
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
  EndBuildGrantRequest,
  MintBuildGrantRequest,
  VolumeRemoval,
  ExecutionError,
  ImageCleanupReport,
  PruneTarget,
  MachineId,
  MachineDetails,
  MachineTarget,
  MachineUpdate,
  MachineUpdated,
  ObservedDataLoss,
  LocalMachineRemoved,
  DataLossConfirmation,
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
  deployment: Parameters<typeof import("./config").lowerDeployment>[0];
  sources: Record<string, string>;
  source_commits?: Record<string, string>;
  /** Uploaded Source content digests, keyed by config.privateDns. A `sources` directory must hold exactly that content; without one only a matching, usable receipt serves it. */
  uploads?: Record<string, string>;
  build_receipts?: BuildReceipts;
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

/** The one Git Service's frozen deployment, the commit to build, and its latest receipt, if any. */
export type OutsideBuildInput = { deployment: PreparationInput["deployment"]; commit: string; receipt?: BuildReceipt };
/** `reuse`: the receipt is for this commit and `machine_name` still holds an image every placement runs. `build`: these platforms. */
export type OutsideBuild =
  | { kind: "reuse"; receipt: BuildReceipt; machine_name: string }
  | { kind: "build"; platforms: string[] };

/** `queued`: not admitted within `startWithinMs`, withdrawn; nothing started. */
export type BuildOutcome = { kind: "queued" } | { kind: "built"; receipt: BuildReceipt };

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
/** Fingerprints a build of these pinned commits and uploads would carry, keyed by Service; no source needed. */
export declare function buildFingerprints(input: Pick<PreparationInput, "deployment" | "uploads"> & { source_commits: Record<string, string> }): Record<string, string>;
/** The ployz version every fingerprint covers; a GitHub runner installs exactly this one. */
export declare function ployzVersion(): string;
/** The tag a Build Grant push retains `digest` under in `repository`, as Image Cleanup knows it. */
export declare function buildGrantTag(repository: string, digest: string): string;
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
  /** What a Builder outside the Cluster does for the one Git Service in `deployment` at `commit`; never builds. */
  outsideBuild(input: OutsideBuildInput): Promise<OutsideBuild>;
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
  /** Mint a Build Grant on this Machine; `grant` is secret and goes only to the pusher. Not retried. */
  mintBuildGrant(request: MintBuildGrantRequest): Promise<BuildGrantMinted>;
  /** Idempotent. `pushed` is the digest this Machine verified; not_found once the grant expired. */
  endBuildGrant(request: EndBuildGrantRequest): Promise<BuildGrantEnded>;
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
  /** One Machine policy edit (Machine Roles and build concurrency); omitted fields keep their values. */
  updateMachine(
    machine: MachineTarget,
    update: Partial<MachineUpdate>,
  ): Promise<MachineUpdated>;
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
   * `trusted` is evidence Cloud gathered itself, such as readable repositories; never the caller's. `principal` is who
   * Cloud authenticated, which Deployments record as their admitter and uploader; none for Cloud itself.
   */
  write(organization: string, command: ConfigCommand, trusted?: ConfigTrusted, principal?: string): Promise<ConfigWritten>;
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
   * Deployments it admitted, or rejects `conflict` when a branch head's `base` is no longer the Store's head.
   */
  system(organization: string, event: SystemEvent, trusted?: ConfigTrusted): Promise<ConfigWritten>;
  /** Cloud's Organization removal only: forget its configuration once it has no Project. */
  removeOrganization(organization: string): Promise<OrganizationRemoved>;
  /** Cloud's sweep only: queued Deployments no runner claimed, admitted before `before` (Unix seconds), oldest first. */
  unclaimed(before: number): Promise<Unclaimed[]>;
  /** Cloud's GitHub workers only: the branch head the Store last saw, which a new head is compared from. */
  branchHead(organization: string, repositoryId: number, branch: string): Promise<string | null>;
  /**
   * Cloud's GitHub workers only: before a push reaches the Store, the pull requests into the branch with a Conditional
   * Save standing (report each that merged first) and the merge commits of frozen ones (pass those the new head
   * contains as the branch head's `merged`).
   */
  pendingSaves(organization: string, repositoryId: number, branch: string): Promise<{ standing: number[]; merged: string[] }>;
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
