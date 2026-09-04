import type {
  ContractDescription,
  DeployEvent,
  DeployIntent,
  DeployOutcome,
  DeployPreview,
  DockerVolumeName,
  ExecutionError,
  MachineId,
  MachineTarget,
  ObservedDataLoss,
  LocalMachineRemoved,
  DataLossConfirmation,
  ClusterTeardown,
  PartialResult,
  PlanOptions,
  ProjectName,
  RegisterRequest,
  Registered,
  RemoveVolumesRequest,
  RequestedServiceSpec,
  RpcError,
  RuntimeWatchFrame,
} from "./generated/payloads";
export * from "./generated/payloads";

export type ConnectOptions = {
  readonly relayUrl: string;
  readonly bearer: string;
  readonly pairing: string;
  readonly machineId: MachineId;
};

export type HeldRegister = {
  readonly machineId: string;
  readonly registerRttNs?: number | null;
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
  readonly finished: Promise<DeployOutcome<ExecutionError>>;
};

export declare function connect(options: ConnectOptions): Promise<Client>;
export declare function listHeld(
  relayUrl: string,
  bearer: string,
  pairing: string,
): Promise<HeldRegister[]>;
export declare function register(
  relayUrl: string,
  bearer: string,
  pairing: string,
  machineId: MachineId,
  identity: RegisterRequest,
): Promise<Registered>;
export declare function revokePairing(
  relayUrl: string,
  bearer: string,
  pairing: string,
): Promise<void>;

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
  about(): Promise<ContractDescription>;
  readonly runtime: {
    watch(options?: WatchOptions): AsyncIterable<RuntimeWatchFrame>;
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
  ): Promise<PartialResult<DockerVolumeName, RpcError>>;
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
