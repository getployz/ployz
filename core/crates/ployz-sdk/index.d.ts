import type {
  ContractDescription,
  DeployEvent,
  DeployIntent,
  DeployOutcome,
  DeployPreview,
  VolumeRemoval,
  ExecutionError,
  MachineId,
  MachineDetails,
  MachineTarget,
  ObservedDataLoss,
  LocalMachineRemoved,
  DataLossConfirmation,
  ClusterTeardown,
  PlanOptions,
  ProjectName,
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
  projectName?: string; serviceId?: string; serviceName?: string; deploymentId?: string;
  machineId?: string; containerId?: string; kind?: "service_container" | "pre_deploy_hook";
};
export type LogRecord = ContainerLogRecord & { id: string };
export type LogSourceError = { type: "source_error"; machineId: string; containerId: string; message: string };
export type LogEvent = { type: "record"; record: LogRecord } | LogSourceError;
export type LogOptions = WatchOptions & { filter?: LogFilter; tail?: number; follow?: boolean };
export type LogHistoryOptions = WatchOptions & { filter?: LogFilter; before: Record<string, string>; limit?: number };
export type LogHistoryPage = { records: LogRecord[]; errors: LogSourceError[] };

export type ConfirmOptions = WatchOptions & { deploymentId?: string };

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
  /** Release unconfirmed retained image resources. */
  close(): void;
  confirm(options?: ConfirmOptions): RunningDeploy;
};

/** Backend-only input. Checkout paths are repository roots, keyed by config.privateDns. */
export type PreparationInput = {
  deployment: Parameters<typeof import("./config").lowerDeployment>[0];
  sources: Record<string, string>;
  source_commits?: Record<string, string>;
  build_receipts?: BuildReceipts;
};

/** One BuildKit step; `id` is stable across repeated reports, timestamps are RFC 3339. */
export type BuildStep = { id: string; name: string; started: string | null; completed: string | null; cached: boolean; error: string | null };

export type PreparationEvent =
  | { Platforms: string[] }
  | { Selected: { machine: import("./generated/payloads").Machine; rejections: string[] } }
  | { Build: { Stage: string } | { Output: number[] } | { Step: BuildStep } | { StepOutput: { step: string; stderr: boolean; text: string } } | { Timing: unknown } | { Target: { name: string; outcome: unknown } } }
  | "Transfer"
  | { Delivered: { image: string; machine_id: MachineId } };

export type RunningPreparation = AsyncIterable<PreparationEvent> & {
  abort(): void;
  /** Completes without consuming progress; failures preserve typed stage and work evidence. */
  readonly finished: Promise<PreparedDeploy>;
};

export type RunningDeploy = AsyncIterable<DeployEvent> & {
  abort(): void;
  /** Rejects with RpcError on session closure; an in-flight mutation may have completed. */
  readonly finished: Promise<DeployOutcome<ExecutionError>>;
};

export declare function connect(options: ConnectOptions): Promise<Client>;
export declare function applyAll(
  project_name: ProjectName,
  specs: readonly RequestedServiceSpec[],
  options?: PlanOptions,
): DeployIntent;

export declare function applyOne(
  project_name: ProjectName,
  spec: RequestedServiceSpec,
  options?: PlanOptions,
): DeployIntent;

export declare class Client {
  prepare(input: PreparationInput, options?: WatchOptions): RunningPreparation;
  clearManagementClient(label: string): Promise<void>;
  inspect(): Promise<MachineDetails>;
  observeEnrollment(): Promise<EnrollmentSnapshot>;
  register(assignment: EnrollmentAssignment): Promise<Registered>;
  about(): Promise<ContractDescription>;
  readonly runtime: {
    watch(options?: WatchOptions): AsyncIterable<RuntimeWatchView>;
    logs(options?: LogOptions): AsyncIterable<LogEvent>;
    logHistory(options: LogHistoryOptions): Promise<LogHistoryPage>;
  };
  preview(intent: DeployIntent): Promise<PreparedDeploy>;
  previewProjectRemoval(
    project_name: ProjectName,
    destroy_volumes: boolean,
  ): Promise<PreparedDeploy>;
  run(
    intent: DeployIntent,
    options?: RunOptions,
  ): Promise<DeployOutcome<ExecutionError>>;
  removeVolumes(
    request: RemoveVolumesRequest,
  ): Promise<VolumeRemoval[]>;
  dataLossIfMachineRemoved(machine: MachineTarget): Promise<ObservedDataLoss>;
  removeMachine(
    machine: MachineTarget,
    confirmDataLoss: DataLossConfirmation,
  ): Promise<LocalMachineRemoved>;
  dataLossIfProjectDestroyed(
    project_name: ProjectName,
    destroy_volumes?: boolean,
  ): Promise<ObservedDataLoss>;
  destroyProject(
    project_name: ProjectName,
    confirmDataLoss: DataLossConfirmation,
    destroy_volumes?: boolean,
  ): Promise<DeployOutcome<ExecutionError>>;
  dataLossIfClusterDestroyed(): Promise<ObservedDataLoss>;
  destroyCluster(confirmDataLoss: DataLossConfirmation): Promise<ClusterTeardown>;
  close(): Promise<void>;
}

export declare function allocateEnrollment(request: RegisterRequest, snapshot: EnrollmentSnapshot, saved: EnrollmentAssignment[]): EnrollmentAssignment;
