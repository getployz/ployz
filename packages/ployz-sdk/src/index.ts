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
  operationEventReplayLimit,
  operationId,
  operationIdempotencyKey,
  replicaCount,
  revisionId,
  serviceId,
} from "./primitives.ts";
import type {
  AcceptedOperation,
  DeploySubmitError,
  DeploySubmitRequest,
  DeploySubmitResponse,
  EventSequence,
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
    },
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
  | PloyzApiError<OpsStatusError>
  | PloyzApiError<OpsWatchError>;
