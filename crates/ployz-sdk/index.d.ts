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
  TailcatRemoval,
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
  RpcError,
  RuntimeWatchView,
} from "./generated/payloads";
export * from "./generated/payloads";

/** Same serialized descriptors as CLI contexts. Backend only: Tailcat is an admin capability. */
export type Connection = (
  | { readonly tailcat: string }
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

export type ConfirmOptions = WatchOptions;

export type RunOptions = WatchOptions;

export declare const RpcError: {
  new (error: RpcError, options?: ErrorOptions): Error & RpcError;
};

export type PreparedDeploy = DeployPreview & {
  readonly noop: boolean;
  confirm(options?: ConfirmOptions): RunningDeploy;
};

export type RunningDeploy = AsyncIterable<DeployEvent> & {
  abort(): void;
  /** Rejects with RpcError on session closure; an in-flight mutation may have completed. */
  readonly finished: Promise<DeployOutcome<ExecutionError>>;
};

export declare function packageName(): "@ployz/sdk";
/** Backend-only ephemeral successor; never save as an ordinary connection candidate. */
export declare function prepareTailcatRemoval(expected: string): Promise<string>;
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
  removeCloudPairing(removal: TailcatRemoval): Promise<void>;
  inspect(): Promise<MachineDetails>;
  observeEnrollment(): Promise<EnrollmentSnapshot>;
  register(assignment: EnrollmentAssignment): Promise<Registered>;
  about(): Promise<ContractDescription>;
  readonly runtime: {
    watch(options?: WatchOptions): AsyncIterable<RuntimeWatchView>;
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
};

export declare function allocateEnrollment(request: RegisterRequest, snapshot: EnrollmentSnapshot, saved: EnrollmentAssignment[]): EnrollmentAssignment;
