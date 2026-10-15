import type * as native from "./generated/native";
import type {
  ConfigQuery,
  ConfigTrusted,
  ConfigView,
  BuildStatus,
  DeployEvent,
  DeployIntent,
  DeployOutcome,
  DeployPreview,
  ExecutionError,
  PruneTarget,
  MachineId,
  MachineTarget,
  MetricHistory,
  MetricHistoryInput,
  ObservedDataLoss,
  DataLossConfirmation,
  PlanOptions,
  Namespace,
  RequestedServiceSpec,
  RpcError as RpcErrorPayload,
  RpcErrorCode,
  RuntimeWatchView,
  ContainerLogRecord,
  InspectVolumeCopyRequest,
  SeedVolumeRequest,
  SetVolumeCopyReply,
  SetVolumeCopyRequest,
  VolumeCopyView,
} from "./generated/payloads";
export * from "./generated/payloads";
export type { CapabilityName } from "./generated/capabilities";
export {
  agentCloudCall,
  agentResume,
  agentStart,
  agentTools,
  allocateEnrollment,
  machineUpgradeAttemptId,
  observeCopies,
  observeVolumes,
  ployzVersion,
} from "./generated/native";

/** One request `Client.volumeSwitch` sends: a Volume copy verb and its payload. */
export type VolumeSwitchRequest =
  | { command: "inspect_volume_copy"; payload: InspectVolumeCopyRequest }
  | { command: "seed_volume"; payload: SeedVolumeRequest }
  | { command: "set_volume_copy"; payload: SetVolumeCopyRequest };

/** The reply payload of one Volume copy verb. */
export type VolumeSwitchReply<C extends VolumeSwitchRequest["command"]> = C extends "set_volume_copy"
  ? SetVolumeCopyReply
  : VolumeCopyView;

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
/** What a line says about itself; a line that says nothing is `info`, stderr included. */
export type LogLevel = "error" | "warn" | "info" | "debug";
export type LogRecord = ContainerLogRecord & { id: string; level: LogLevel };
/** `capability` names what the Server lacks. */
export type LogSourceError = {
  type: "source_error"; machineId: string; containerId: string; message: string;
  code?: RpcErrorCode; capability?: string;
};
/** A container whose source failed has left the Runtime Watch; nothing more will come from it. */
export type LogSourceGone = { type: "source_gone"; machineId: string; containerId: string };
export type LogEvent = { type: "record"; record: LogRecord } | LogSourceError | LogSourceGone;
export type LogOptions = WatchOptions & { filter?: LogFilter; tail?: number; follow?: boolean };
/** A stretch of one container's output its Server's Log Store doesn't have. */
export type LogGap = {
  machineId: string; machineName: string; containerId: string; serviceName: string;
  fromNanos: string; toNanos: string; reason: "not_captured" | "corrupt";
};
/** A container stopping; `exitCode` is null when Docker didn't say. */
export type LogExit = {
  machineId: string; machineName: string; containerId: string; serviceName: string;
  timestampNanos: string; exitCode: number | null; oomKilled: boolean;
};
/** A Server whose Log Store didn't answer, and why. */
export type LogMachineFailure = { machineId: string; machineName: string; message: string; code?: RpcErrorCode; capability?: string };
/** Without `cursor`, the first page starts at each Server's newest row, or before `before` (Unix nanoseconds). */
export type LogHistoryOptions = WatchOptions & { filter: LogFilter; cursor?: string; before?: string; limit?: number };
/** Newest first across every Server; `cursor` is null once none has anything older. */
export type LogHistoryPage = { records: LogRecord[]; gaps: LogGap[]; exits: LogExit[]; failures: LogMachineFailure[]; cursor: string | null };

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
    /** Services this targeted Deploy retires, each also in `selected`. */
    removals?: import("./generated/payloads").ServiceName[];
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
/** The `SetVolumeCopyRefused` a `volumeSwitch` rejection carries in its `details`, or null for any other error. */
export declare function setVolumeCopyRefusal(error: unknown): import("./generated/payloads").SetVolumeCopyRefused | null;
/** A call's state between steps. Keep it and hand it back unchanged; only the step machine reads it. */
export type AgentState = { readonly __brand: "AgentState" };
export type AgentTurn = { step: import("./generated/payloads").Step; state: AgentState };
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

type WrappedClientMethods =
  | "containerLogs" | "logHistory" | "metricHistory" | "watch"
  | "prepare" | "build" | "preview" | "previewNamespaceRemoval" | "volumeSwitch"
  | "dataLossIfNamespaceDestroyed" | "destroyNamespace";

export interface Client extends Omit<native.Client, WrappedClientMethods> {
  prepare(input: PreparationInput, options?: WatchOptions): RunningPreparation;
  /** One Image Build. `input` holds exactly one Git Service with its checkout and commit; its receipt is a reuse hint. */
  build(input: PreparationInput, options?: BuildOptions): RunningBuild;
  readonly runtime: {
    watch(options?: WatchOptions): AsyncIterable<RuntimeWatchView>;
    logs(options?: LogOptions): AsyncIterable<LogEvent>;
    logHistory(options: LogHistoryOptions): Promise<LogHistoryPage>;
    /** Sums a Service across the Machines that answered; names each Machine that did not. */
    metricHistory(input: MetricHistoryInput): Promise<MetricHistory>;
  };
  preview(intent: DeployIntent): Promise<PreparedDeploy>;
  previewNamespaceRemoval(namespace: Namespace, destroy_volumes: boolean): Promise<PreparedDeploy>;
  run(intent: DeployIntent, options?: RunOptions): Promise<DeployOutcome<ExecutionError>>;
  /** A `set_volume_copy` refusal rejects with an `RpcError` whose `details` is a `SetVolumeCopyRefused`. */
  volumeSwitch<R extends VolumeSwitchRequest>(machine: MachineTarget, request: R): Promise<VolumeSwitchReply<R["command"]>>;
  dataLossIfNamespaceDestroyed(namespace: Namespace, destroy_volumes?: boolean): Promise<ObservedDataLoss>;
  destroyNamespace(
    namespace: Namespace,
    confirmDataLoss: DataLossConfirmation,
    destroy_volumes?: boolean,
  ): Promise<DeployOutcome<ExecutionError>>;
}
export declare const Client: { new (inner: native.Client): Client; prototype: Client };

/** One Config Store; every call acts in one Organization and rejects with RpcError. */
export interface ConfigStore extends Omit<native.ConfigStore, "read"> {
  /** The view of the same name as the query. */
  read<Q extends ConfigQuery>(organization: string, query: Q, trusted?: ConfigTrusted): Promise<Extract<ConfigView, { view: Q["query"] }>>;
}
export type GithubStart = { kind: "reused" } | { kind: "dispatch"; runner: string } | { kind: "skipped"; message: string };
export type GithubCheckIn = { grant: string; commit: string; fingerprint: string; ployzVersion: string; deployment: PreparationInput["deployment"] };
export type GithubFinish = "waiting" | { ended: BuildStatus };
/** Calls run off the JavaScript thread, a few at once; a slow or queued call rejects `unavailable`. */
export declare function openConfigStore(url: string, sealingSecret: string): Promise<ConfigStore>;
