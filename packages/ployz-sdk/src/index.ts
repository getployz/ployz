export type * from "./generated.ts";
export {
  MAX_OPERATION_EVENT_REPLAY_LIMIT,
  OPERATION_API_CONTRACTS,
} from "./generated.ts";
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
  imageReference,
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
  DeploySubmitError,
  DeploySubmitRequest,
  DeploySubmitResponse,
  EventSequence,
  MachineAddAccepted,
  MachineAddError,
  MachineAddGateway,
  MachineAddRequest,
  MachineAddResponse,
  MachineJoinBundle,
  MachineJoinRedeemRequest,
  MachineJoinRedeemResponse,
  MachineJoinRedeemed,
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
} from "./generated.ts";

export interface PloyzOperationTransport {
  deploySubmit(request: DeploySubmitRequest): Promise<DeploySubmitResponse>;
  machineAdd(request: MachineAddRequest): Promise<MachineAddResponse>;
  machineJoinRedeem(request: MachineJoinRedeemRequest): Promise<MachineJoinRedeemResponse>;
  opsStatus(request: OpsStatusRequest): Promise<OpsStatusResponse>;
  opsWatch(request: OpsWatchRequest): Promise<OpsWatchResponse>;
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
  };
}

export interface PloyzMachineAddInput {
  operationId: string;
  idempotencyKey: string;
  nodeId: string;
  name: string;
  gateway: MachineAddGateway;
  joinBundle: MachineJoinBundle;
}

export interface PloyzMachineJoinRedeemInput {
  joinToken: string;
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

  async machineAdd(input: PloyzMachineAddInput): Promise<MachineAddHandle> {
    const accepted = unwrapApiResponse(
      "machine.add",
      await this.#transport.machineAdd(machineAddRequest(input)),
    );
    return new MachineAddHandle(this.#transport, accepted);
  }

  async machineJoinRedeem(input: PloyzMachineJoinRedeemInput): Promise<MachineJoinRedeemed> {
    return unwrapApiResponse(
      "machine.join.redeem",
      await this.#transport.machineJoinRedeem(machineJoinRedeemRequest(input)),
    );
  }
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
              hostname: routeHostname(input.route.hostname),
              port: routePort(input.route.port),
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
    join_bundle: input.joinBundle,
  };
}

export function machineJoinRedeemRequest(
  input: PloyzMachineJoinRedeemInput,
): MachineJoinRedeemRequest {
  return {
    join_token: machineJoinToken(input.joinToken),
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
  | PloyzApiError<DeploySubmitError>
  | PloyzApiError<MachineAddError>
  | PloyzApiError<OpsStatusError>
  | PloyzApiError<OpsWatchError>;
