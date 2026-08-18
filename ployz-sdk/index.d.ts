import type {
  ContractDescription,
  DeployIntent,
  DeployOutcome,
  ExecutionError,
  MachineId,
} from "./generated/payloads";
export * from "./generated/payloads";

export type ConnectOptions = {
  readonly relayUrl: string;
  readonly bearer: string;
  readonly machineId: MachineId;
};

export declare function connect(options: ConnectOptions): Promise<Client>;

export declare class Client {
  about(): Promise<ContractDescription>;
  deploy(intent: DeployIntent): Promise<DeployOutcome<ExecutionError>>;
  close(): Promise<void>;
};
