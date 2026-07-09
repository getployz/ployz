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
  containerMountPath,
  eventSequence,
  failureMessage,
  imageReference,
  installArtifactVersion,
  logsTailLines,
  machineBootstrapUrl,
  machineJoinToken,
  machineName,
  machineId,
  namespaceId,
  operationEventReplayLimit,
  operationId,
  operationIdempotencyKey,
  operatorHint,
  namespaceRevisionEntryId,
  namespaceRevisionId,
  replicaCount,
  routeHostname,
  routePort,
  serviceId,
  volumeName,
} from "./primitives.ts";

import {
  containerId,
  imageReference,
  installArtifactVersion,
  logsTailLines,
  machineName,
  machineJoinToken,
  machineId,
  namespaceId,
  operationEventReplayLimit,
  operationId,
  operationIdempotencyKey,
  replicaCount,
  routeHostname,
  routePort,
  serviceId,
  stopGracePeriod,
} from "./primitives.ts";
import type {
  AcceptedOperation,
  ContainerRuntimeSpec,
  DeploySubmitError,
  DeploySubmitRequest,
  EventSequence,
  InitFirstMachineActivateError,
  InitFirstMachineActivateRequest,
  InitFirstMachineActivated,
  LogsTailError,
  LogsTailLines,
  LogsTailRequest,
  LogsTailResult,
  MachineAddAccepted,
  MachineAddError,
  MachineAddRequest,
  InstallRolePolicy,
  MachineJoinRedeemError,
  MachineJoinRedeemRequest,
  MachineJoinRedeemed,
  MachineInspectError,
  MachineInspectRequest,
  MachineListError,
  MachineListRequest,
  MachineSnapshot,
  MachineUpdateError,
  MachineUpdateRequest,
  OperationApiResponse,
  OperationApiRequestByEndpoint,
  OperationApiResponseByEndpoint,
  OperationEventReplayCursor,
  OperationEventReplayLimit,
  OperationEventReplayPage,
  OperationId,
  OperationStatusSnapshot,
  OpsListError,
  OpsListRequest,
  OpsListResult,
  OpsStatusError,
  OpsWatchError,
  RuntimeSnapshotError,
  RuntimeSnapshot,
  RuntimeSnapshotRequest,
  ServiceInspectError,
  ServiceInspectRequest,
  ServiceListError,
  ServiceListRequest,
  ServiceSnapshot,
  PloyzApiEndpoint,
} from "./generated.ts";

export interface PloyzOperationTransport {
  request<E extends PloyzApiEndpoint>(
    endpoint: E,
    request: OperationApiRequestByEndpoint[E],
  ): Promise<OperationApiResponseByEndpoint[E]>;
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

export interface PloyzDeployInput {
  idempotencyKey: string;
  namespaceId?: string;
  serviceId: string;
  image: string;
  replicas: number;
  routes?: Array<{
    hostname: string;
    port: number;
    endpointPort: number;
  }>;
}

export interface PloyzMachineAddInput {
  operationId: string;
  idempotencyKey: string;
  machineId: string;
  name: string;
  roles: InstallRolePolicy;
}

export interface PloyzMachineUpdateInput {
  operationId: string;
  machineId: string;
  targetVersion: string;
}

export interface PloyzFirstMachineActivateInput {
  machineId: string;
  roles: InstallRolePolicy;
}

export interface PloyzMachineJoinRedeemInput {
  joinToken: string;
}

export interface PloyzMachineInspectInput {
  machineId: string;
}

export interface PloyzServiceInspectInput {
  namespaceId?: string;
  serviceId: string;
}

export interface PloyzLogsTailInput {
  containerId: string;
  machineId?: string;
  tailLines?: number | LogsTailLines;
}

export interface PloyzOpsListInput {
  activeOnly?: boolean;
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
      await this.#transport.request("deploy.submit", request),
    );
    return new OperationHandle(this.#transport, accepted);
  }

  async initFirstMachineActivate(
    input: PloyzFirstMachineActivateInput,
  ): Promise<FirstMachineActivationHandle> {
    const activated = unwrapApiResponse(
      "init.first_machine.activate",
      await this.#transport.request(
        "init.first_machine.activate",
        initFirstMachineActivateRequest(input),
      ),
    );
    return new FirstMachineActivationHandle(this.#transport, activated);
  }

  async machineAdd(input: PloyzMachineAddInput): Promise<MachineAddHandle> {
    const accepted = unwrapApiResponse(
      "machine.add",
      await this.#transport.request("machine.add", machineAddRequest(input)),
    );
    return new MachineAddHandle(this.#transport, accepted);
  }

  async machineUpdate(input: PloyzMachineUpdateInput): Promise<OperationHandle> {
    const accepted = unwrapApiResponse(
      "machine.update",
      await this.#transport.request("machine.update", machineUpdateRequest(input)),
    );
    return new OperationHandle(this.#transport, accepted);
  }

  async machineList(): Promise<MachineSnapshot[]> {
    return unwrapApiResponse(
      "machine.list",
      await this.#transport.request("machine.list", machineListRequest()),
    ).machines;
  }

  async machineInspect(input: string | PloyzMachineInspectInput): Promise<MachineSnapshot> {
    return unwrapApiResponse(
      "machine.inspect",
      await this.#transport.request("machine.inspect", machineInspectRequest(input)),
    );
  }

  async machineJoinRedeem(input: PloyzMachineJoinRedeemInput): Promise<MachineJoinRedeemed> {
    return unwrapApiResponse(
      "machine.redeem",
      await this.#transport.request("machine.redeem", machineJoinRedeemRequest(input)),
    );
  }

  async serviceList(): Promise<ServiceSnapshot[]> {
    return unwrapApiResponse(
      "service.list",
      await this.#transport.request("service.list", serviceListRequest()),
    ).services;
  }

  async serviceInspect(input: string | PloyzServiceInspectInput): Promise<ServiceSnapshot> {
    return unwrapApiResponse(
      "service.inspect",
      await this.#transport.request("service.inspect", serviceInspectRequest(input)),
    );
  }

  async runtimeSnapshot(): Promise<RuntimeSnapshot> {
    return unwrapApiResponse(
      "runtime.snapshot",
      await this.#transport.request("runtime.snapshot", runtimeSnapshotRequest()),
    ).snapshot;
  }

  async logsTail(input: string | PloyzLogsTailInput): Promise<LogsTailResult> {
    return unwrapApiResponse(
      "logs.tail",
      await this.#transport.request("logs.tail", logsTailRequest(input)),
    );
  }

  async opsList(input: PloyzOpsListInput = {}): Promise<OpsListResult> {
    return unwrapApiResponse(
      "ops.list",
      await this.#transport.request("ops.list", opsListRequest(input)),
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

/**
 * Image defaults: no command/entrypoint overrides, empty environment, and the
 * runtime's default stop grace period. Mirrors ContainerRuntimeSpec::image_defaults().
 */
export function imageDefaultRuntime(): ContainerRuntimeSpec {
  return {
    command: null,
    entrypoint: null,
    environment: {},
    stop_grace_period: stopGracePeriod(10),
    volume_mounts: [],
  };
}

export function deploySubmitRequest(input: PloyzDeployInput): DeploySubmitRequest {
  return {
    idempotency_key: operationIdempotencyKey(input.idempotencyKey),
    target: {
      namespace_id: namespaceId(input.namespaceId ?? "default"),
      services: [
        {
          service_id: serviceId(input.serviceId),
          image: imageReference(input.image),
          replicas: replicaCount(input.replicas),
          runtime: imageDefaultRuntime(),
          routes: (input.routes ?? []).map((route) => ({
            target: {
              kind: "hostname" as const,
              hostname: routeHostname(route.hostname),
              port: routePort(route.port),
            },
            endpoint_port: routePort(route.endpointPort),
          })),
        },
      ],
    },
  };
}

export function machineAddRequest(input: PloyzMachineAddInput): MachineAddRequest {
  return {
    operation_id: operationId(input.operationId),
    idempotency_key: operationIdempotencyKey(input.idempotencyKey),
    machine_id: machineId(input.machineId),
    name: machineName(input.name),
    roles: input.roles,
  };
}

export function machineUpdateRequest(input: PloyzMachineUpdateInput): MachineUpdateRequest {
  return {
    operation_id: operationId(input.operationId),
    machine_id: machineId(input.machineId),
    target_version: installArtifactVersion(input.targetVersion),
  };
}

export function initFirstMachineActivateRequest(
  input: PloyzFirstMachineActivateInput,
): InitFirstMachineActivateRequest {
  return {
    machine_id: machineId(input.machineId),
    roles: input.roles,
  };
}

export function machineListRequest(): MachineListRequest {
  return {};
}

export function machineInspectRequest(
  input: string | PloyzMachineInspectInput,
): MachineInspectRequest {
  return {
    machine_id: machineId(typeof input === "string" ? input : input.machineId),
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
  const namespace = typeof input === "string" ? undefined : input.namespaceId;
  return {
    namespace_id: namespaceId(namespace ?? "default"),
    service_id: serviceId(typeof input === "string" ? input : input.serviceId),
  };
}

export function runtimeSnapshotRequest(): RuntimeSnapshotRequest {
  return {};
}

export function logsTailRequest(input: string | PloyzLogsTailInput): LogsTailRequest {
  if (typeof input === "string") {
    return {
      target: { target: "container" as const, container_id: containerId(input) },
    };
  }

  return {
    target: {
      target: "container" as const,
      container_id: containerId(input.containerId),
      ...(input.machineId ? { machine_id: machineId(input.machineId) } : {}),
    },
    ...(input.tailLines === undefined ? {} : { tail_lines: logsTailLines(input.tailLines) }),
  };
}

export function opsListRequest(input: PloyzOpsListInput = {}): OpsListRequest {
  return {
    active_only: input.activeOnly ?? false,
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
      await this.#transport.request("ops.status", {
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
      await this.#transport.request("ops.watch", {
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

  get machineId(): MachineAddAccepted["machine_id"] {
    return this.machine.machine_id;
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

export class FirstMachineActivationHandle {
  readonly #transport: PloyzOperationTransport;
  readonly activated: InitFirstMachineActivated;

  constructor(transport: PloyzOperationTransport, activated: InitFirstMachineActivated) {
    this.#transport = transport;
    this.activated = activated;
  }

  get operationId(): OperationId {
    return this.activated.operation_id;
  }

  get machineId(): InitFirstMachineActivated["machine_id"] {
    return this.activated.machine_id;
  }

  async status(): Promise<OperationStatusSnapshot> {
    return unwrapApiResponse(
      "ops.status",
      await this.#transport.request("ops.status", {
        operation_id: this.activated.operation_id,
      }),
    );
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
  | PloyzApiError<InitFirstMachineActivateError>
  | PloyzApiError<MachineAddError>
  | PloyzApiError<MachineUpdateError>
  | PloyzApiError<MachineListError>
  | PloyzApiError<MachineInspectError>
  | PloyzApiError<MachineJoinRedeemError>
  | PloyzApiError<LogsTailError>
  | PloyzApiError<OpsListError>
  | PloyzApiError<RuntimeSnapshotError>
  | PloyzApiError<ServiceListError>
  | PloyzApiError<ServiceInspectError>
  | PloyzApiError<OpsStatusError>
  | PloyzApiError<OpsWatchError>;
