import type { ContractDescription, MachineId } from "./generated/payloads";
export * from "./generated/payloads";

export type ConnectOptions = {
  readonly relayUrl: string;
  readonly bearer: string;
  readonly machineId: MachineId;
};

export declare function connect(options: ConnectOptions): Promise<Client>;

export declare class Client {
  about(): Promise<ContractDescription>;
  close(): Promise<void>;
};
