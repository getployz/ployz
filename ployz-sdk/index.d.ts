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
  DataLoss,
  PartialResult,
  PlanOptions,
  ProjectName,
  RemoveVolumesRequest,
  RequestedServiceSpec,
  RpcError,
  RuntimeWatchFrame,
} from "./generated/payloads";
export * from "./generated/payloads";

export type ConnectOptions = {
  readonly relayUrl: string;
  readonly bearer: string;
  readonly machineId: MachineId;
};

export type WatchOptions = {
  readonly signal?: AbortSignal;
};

export type ConfirmOptions = WatchOptions;

export type RunOptions = WatchOptions;

export type PreparedDeploy = DeployPreview & {
  readonly noop: boolean;
  confirm(options?: ConfirmOptions): RunningDeploy;
};

export type RunningDeploy = AsyncIterable<DeployEvent> & {
  abort(): void;
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
    confirmDataLoss: DataLoss[],
  ): Promise<LocalMachineRemoved>;
  close(): Promise<void>;
};
