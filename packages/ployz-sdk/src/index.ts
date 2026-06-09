import {
  connectPloyzNatsTransport,
  type PloyzNatsConnectOptions,
  type PloyzNatsTransport,
} from "./nats.ts";

export type * from "./generated.ts";
export {
  MAX_LOGS_TAIL_LINES,
  MAX_OPERATION_EVENT_REPLAY_LIMIT,
  OPERATION_API_CONTRACTS,
} from "./generated.ts";
export * from "./nats.ts";
export {
  acmeChallengeToken,
  acmeChallengeTtlSeconds,
  acmeChallengeValue,
  cancellationReason,
  certId,
  certBundleRef,
  certValidAt,
  containerId,
  eventSequence,
  failureMessage,
  imageReference,
  logsTailLines,
  machineBootstrapUrl,
  machineJoinToken,
  machineName,
  nodeId,
  operationEventReplayLimit,
  operationId,
  operationIdempotencyKey,
  operationLeaseExpiresAt,
  operationOwnerId,
  operatorHint,
  replicaCount,
  revisionId,
  routeHostname,
  routePort,
  serviceId,
} from "./primitives.ts";

import {
  OPERATION_API_CONTRACTS,
} from "./generated.ts";
import {
  containerId,
  imageReference,
  logsTailLines,
  machineName,
  machineJoinToken,
  nodeId,
  operationEventReplayLimit,
  operationId,
  operationIdempotencyKey,
  replicaCount,
  revisionId,
  routeHostname,
  routePort,
  serviceId,
} from "./primitives.ts";
import type {
  AcceptedOperation,
  BackupCreateError,
  BackupCreateRequest,
  BackupCreateResponse,
  DeploySubmitError,
  DeploySubmitRequest,
  DeploySubmitResponse,
  EventSequence,
  LogsTailError,
  LogsTailLines,
  LogsTailRequest,
  LogsTailResponse,
  LogsTailResult,
  MachineAddAccepted,
  MachineAddError,
  MachineAddGateway,
  MachineAddRequest,
  MachineAddResponse,
  MachineJoinBundle,
  MachineJoinSecretDelivery,
  MachineJoinRedeemError,
  MachineJoinRedeemRequest,
  MachineJoinRedeemResponse,
  MachineJoinRedeemed,
  MachineJoinReportRequest,
  MachineJoinReportResponse,
  MachineInspectError,
  MachineInspectRequest,
  MachineInspectResponse,
  MachineListError,
  MachineListRequest,
  MachineListResponse,
  MachineSnapshot,
  OperationApiResponse,
  OperationEventReplayCursor,
  OperationEventReplayLimit,
  OperationEventReplayPage,
  OperationId,
  OperationStatusSnapshot,
  OpsStatusError,
  OpsStatusRequest,
  OpsStatusResponse,
  OpsWatchError,
  OpsWatchRequest,
  OpsWatchResponse,
  ServiceInspectError,
  ServiceInspectRequest,
  ServiceInspectResponse,
  ServiceListError,
  ServiceListRequest,
  ServiceListResponse,
  ServiceSnapshot,
} from "./generated.ts";

export interface PloyzOperationTransport {
  deploySubmit(request: DeploySubmitRequest): Promise<DeploySubmitResponse>;
  backupCreate(request: BackupCreateRequest): Promise<BackupCreateResponse>;
  machineAdd(request: MachineAddRequest): Promise<MachineAddResponse>;
  machineList(request: MachineListRequest): Promise<MachineListResponse>;
  machineInspect(request: MachineInspectRequest): Promise<MachineInspectResponse>;
  machineJoinRedeem(request: MachineJoinRedeemRequest): Promise<MachineJoinRedeemResponse>;
  machineJoinReport(request: MachineJoinReportRequest): Promise<MachineJoinReportResponse>;
  serviceList(request: ServiceListRequest): Promise<ServiceListResponse>;
  serviceInspect(request: ServiceInspectRequest): Promise<ServiceInspectResponse>;
  opsStatus(request: OpsStatusRequest): Promise<OpsStatusResponse>;
  opsWatch(request: OpsWatchRequest): Promise<OpsWatchResponse>;
  logsTail(request: LogsTailRequest): Promise<LogsTailResponse>;
}

export class PloyzApiError<E> extends Error {
  readonly endpoint: PloyzApiEndpoint;
  readonly error: E;

  constructor(endpoint: PloyzApiEndpoint, error: E) {
    super(`${endpoint} returned a domain error`);
    this.name = "PloyzApiError";
    this.endpoint = endpoint;
    this.error = error;
  }
}

export type PloyzApiEndpoint = (typeof OPERATION_API_CONTRACTS)[number]["name"];

export interface PloyzDeployInput {
  operationId: string;
  idempotencyKey: string;
  serviceId: string;
  targetRevision: string;
  image: string;
  replicas: number;
  route?: {
    hostname: string;
    port: number;
    endpointPort: number;
  };
}

export interface PloyzMachineAddInput {
  operationId: string;
  idempotencyKey: string;
  nodeId: string;
  name: string;
  gateway: MachineAddGateway;
}

export interface PloyzMachineJoinRedeemInput {
  joinToken: string;
}

export interface PloyzBackupCreateInput {
  operationId: string;
  idempotencyKey: string;
}

export interface PloyzMachineInspectInput {
  nodeId: string;
}

export interface PloyzServiceInspectInput {
  serviceId: string;
}

export interface PloyzLogsTailInput {
  containerId: string;
  nodeId?: string;
  tailLines?: number | LogsTailLines;
}

export class PloyzClient {
  readonly #transport: PloyzOperationTransport;

  constructor(transport: PloyzOperationTransport) {
    this.#transport = transport;
  }

  async deploy(input: PloyzDeployInput): Promise<OperationHandle> {
    const request = deploySubmitRequest(input);
    const accepted = unwrapApiResponse(
      "deploy.submit",
      await this.#transport.deploySubmit(request),
    );
    return new OperationHandle(this.#transport, accepted);
  }

  async backupCreate(input: PloyzBackupCreateInput): Promise<OperationHandle> {
    const accepted = unwrapApiResponse(
      "backup.create",
      await this.#transport.backupCreate(backupCreateRequest(input)),
    );
    return new OperationHandle(this.#transport, accepted);
  }

  async machineAdd(input: PloyzMachineAddInput): Promise<MachineAddHandle> {
    const accepted = unwrapApiResponse(
      "machine.add",
      await this.#transport.machineAdd(machineAddRequest(input)),
    );
    return new MachineAddHandle(this.#transport, accepted);
  }

  async machineList(): Promise<MachineSnapshot[]> {
    return unwrapApiResponse(
      "machine.list",
      await this.#transport.machineList(machineListRequest()),
    ).machines;
  }

  async machineInspect(input: string | PloyzMachineInspectInput): Promise<MachineSnapshot> {
    return unwrapApiResponse(
      "machine.inspect",
      await this.#transport.machineInspect(machineInspectRequest(input)),
    );
  }

  async machineJoinRedeem(input: PloyzMachineJoinRedeemInput): Promise<MachineJoinRedeemed> {
    return unwrapApiResponse(
      "machine.join.redeem",
      await this.#transport.machineJoinRedeem(machineJoinRedeemRequest(input)),
    );
  }

  async serviceList(): Promise<ServiceSnapshot[]> {
    return unwrapApiResponse(
      "service.list",
      await this.#transport.serviceList(serviceListRequest()),
    ).services;
  }

  async serviceInspect(input: string | PloyzServiceInspectInput): Promise<ServiceSnapshot> {
    return unwrapApiResponse(
      "service.inspect",
      await this.#transport.serviceInspect(serviceInspectRequest(input)),
    );
  }

  async logsTail(input: string | PloyzLogsTailInput): Promise<LogsTailResult> {
    return unwrapApiResponse(
      "logs.tail",
      await this.#transport.logsTail(logsTailRequest(input)),
    );
  }
}

export class ConnectedPloyzClient {
  readonly client: PloyzClient;
  readonly transport: PloyzNatsTransport;

  constructor(transport: PloyzNatsTransport) {
    this.transport = transport;
    this.client = new PloyzClient(transport);
  }

  close(): Promise<void> {
    return this.transport.close();
  }

  drain(): Promise<void> {
    return this.transport.drain();
  }
}

export async function connectPloyzNatsClient(
  options: PloyzNatsConnectOptions = {},
): Promise<ConnectedPloyzClient> {
  return new ConnectedPloyzClient(await connectPloyzNatsTransport(options));
}

export async function connectPloyzNats(
  options: PloyzNatsConnectOptions = {},
): Promise<PloyzClient> {
  return (await connectPloyzNatsClient(options)).client;
}

export function backupCreateRequest(input: PloyzBackupCreateInput): BackupCreateRequest {
  return {
    operation_id: operationId(input.operationId),
    idempotency_key: operationIdempotencyKey(input.idempotencyKey),
  };
}

export function deploySubmitRequest(input: PloyzDeployInput): DeploySubmitRequest {
  return {
    operation_id: operationId(input.operationId),
    idempotency_key: operationIdempotencyKey(input.idempotencyKey),
    target: {
      service_id: serviceId(input.serviceId),
      target_revision: revisionId(input.targetRevision),
      image: imageReference(input.image),
      replicas: replicaCount(input.replicas),
      ...(input.route
        ? {
            route: {
              target: {
                hostname: routeHostname(input.route.hostname),
                port: routePort(input.route.port),
              },
              endpoint_port: routePort(input.route.endpointPort),
            },
          }
        : {}),
    },
  };
}

export function machineAddRequest(input: PloyzMachineAddInput): MachineAddRequest {
  return {
    operation_id: operationId(input.operationId),
    idempotency_key: operationIdempotencyKey(input.idempotencyKey),
    node_id: nodeId(input.nodeId),
    name: machineName(input.name),
    gateway: input.gateway,
  };
}

export function machineListRequest(): MachineListRequest {
  return {};
}

export function machineInspectRequest(
  input: string | PloyzMachineInspectInput,
): MachineInspectRequest {
  return {
    node_id: nodeId(typeof input === "string" ? input : input.nodeId),
  };
}

export function machineJoinRedeemRequest(
  input: PloyzMachineJoinRedeemInput,
): MachineJoinRedeemRequest {
  return {
    join_token: machineJoinToken(input.joinToken),
  };
}

export function serviceListRequest(): ServiceListRequest {
  return {};
}

export function serviceInspectRequest(
  input: string | PloyzServiceInspectInput,
): ServiceInspectRequest {
  return {
    service_id: serviceId(typeof input === "string" ? input : input.serviceId),
  };
}

export function logsTailRequest(input: string | PloyzLogsTailInput): LogsTailRequest {
  if (typeof input === "string") {
    return {
      container_id: containerId(input),
    };
  }

  return {
    container_id: containerId(input.containerId),
    ...(input.nodeId ? { node_id: nodeId(input.nodeId) } : {}),
    ...(input.tailLines === undefined ? {} : { tail_lines: logsTailLines(input.tailLines) }),
  };
}

export class OperationHandle {
  readonly #transport: PloyzOperationTransport;
  readonly accepted: AcceptedOperation;

  constructor(transport: PloyzOperationTransport, accepted: AcceptedOperation) {
    this.#transport = transport;
    this.accepted = accepted;
  }

  get operationId(): OperationId {
    return this.accepted.operation_id;
  }

  async status(): Promise<OperationStatusSnapshot> {
    return unwrapApiResponse(
      "ops.status",
      await this.#transport.opsStatus({
        operation_id: this.accepted.operation_id,
      }),
    );
  }

  replayFromStart(limit: number): Promise<OperationEventReplayPage> {
    return this.replayFrom(this.accepted.start_sequence, limit);
  }

  async replayFrom(
    startSequence: EventSequence,
    limit: number | OperationEventReplayLimit,
  ): Promise<OperationEventReplayPage> {
    return unwrapApiResponse(
      "ops.watch",
      await this.#transport.opsWatch({
        operation_id: this.accepted.operation_id,
        start_sequence: startSequence,
        limit: operationEventReplayLimit(limit),
      }),
    );
  }

  async *replayPages(limit: number): AsyncGenerator<OperationEventReplayPage> {
    let cursor: OperationEventReplayCursor = {
      state: "more",
      next_start_sequence: this.accepted.start_sequence,
    };

    while (cursor.state === "more") {
      const page = await this.replayFrom(cursor.next_start_sequence, limit);
      yield page;
      cursor = page.cursor;
    }
  }
}

export class MachineAddHandle extends OperationHandle {
  readonly machine: MachineAddAccepted;

  constructor(transport: PloyzOperationTransport, machine: MachineAddAccepted) {
    super(transport, machine.accepted);
    this.machine = machine;
  }

  get nodeId(): MachineAddAccepted["node_id"] {
    return this.machine.node_id;
  }

  get bootstrapUrl(): MachineAddAccepted["bootstrap_url"] {
    return this.machine.bootstrap_url;
  }

  get joinBundle(): MachineAddAccepted["join_bundle"] {
    return this.machine.join_bundle;
  }

  get runtimeNatsUrl(): MachineAddAccepted["join_bundle"]["material"]["runtime_nats_url"] {
    return this.machine.join_bundle.material.runtime_nats_url;
  }

  get joinToken(): MachineAddAccepted["join_token"] {
    return this.machine.join_token;
  }
}

function unwrapApiResponse<T, E>(
  endpoint: PloyzApiEndpoint,
  response: OperationApiResponse<T, E>,
): T {
  switch (response.status) {
    case "ok":
      return response.value;
    case "domain_error":
      throw new PloyzApiError(endpoint, response.error);
  }
}

export type PloyzOperationError =
  | PloyzApiError<BackupCreateError>
  | PloyzApiError<DeploySubmitError>
  | PloyzApiError<MachineAddError>
  | PloyzApiError<MachineListError>
  | PloyzApiError<MachineInspectError>
  | PloyzApiError<MachineJoinRedeemError>
  | PloyzApiError<LogsTailError>
  | PloyzApiError<ServiceListError>
  | PloyzApiError<ServiceInspectError>
  | PloyzApiError<OpsStatusError>
  | PloyzApiError<OpsWatchError>;
