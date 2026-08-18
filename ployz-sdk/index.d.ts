import type {
  ContractDescription,
  DeployIntent,
  DeployOutcome,
  ExecutionError,
  MachineId,
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
  close(): Promise<void>;
};
