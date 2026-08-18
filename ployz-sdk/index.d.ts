import type {
  ContractDescription,
  DeployIntent,
  DeployOutcome,
  DockerVolumeName,
  ExecutionError,
  MachineId,
  PartialResult,
  RemoveVolumesRequest,
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

export declare function connect(options: ConnectOptions): Promise<Client>;

export declare class Client {
  about(): Promise<ContractDescription>;
  readonly runtime: {
    watch(options?: WatchOptions): AsyncIterable<RuntimeWatchFrame>;
  };
  deploy(intent: DeployIntent): Promise<DeployOutcome<ExecutionError>>;
  removeVolumes(
    request: RemoveVolumesRequest,
  ): Promise<PartialResult<DockerVolumeName, RpcError>>;
  close(): Promise<void>;
};
