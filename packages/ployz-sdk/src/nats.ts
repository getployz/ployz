import {
  connect as connectNats,
  type NodeConnectionOptions,
} from "@nats-io/transport-node";

import { OPERATION_API_CONTRACTS } from "./generated.ts";
import type {
  BackupCreateRequest,
  BackupCreateResponse,
  DeploySubmitRequest,
  DeploySubmitResponse,
  MachineAddRequest,
  MachineAddResponse,
  MachineInspectRequest,
  MachineInspectResponse,
  MachineJoinRedeemRequest,
  MachineJoinRedeemResponse,
  MachineJoinReportRequest,
  MachineJoinReportResponse,
  MachineListRequest,
  MachineListResponse,
  OpsStatusRequest,
  OpsStatusResponse,
  OpsWatchRequest,
  OpsWatchResponse,
  ServiceInspectRequest,
  ServiceInspectResponse,
  ServiceListRequest,
  ServiceListResponse,
} from "./generated.ts";

const DEFAULT_NATS_REQUEST_TIMEOUT_MS = 10_000;
const NATS_SERVICE_ERROR_HEADER = "Nats-Service-Error";
const NATS_SERVICE_ERROR_CODE_HEADER = "Nats-Service-Error-Code";

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

type PloyzApiEndpoint = (typeof OPERATION_API_CONTRACTS)[number]["name"];

export interface PloyzNatsConnectOptions {
  nats?: NodeConnectionOptions;
  requestTimeoutMs?: number;
}

export interface PloyzNatsTransportOptions {
  requestTimeoutMs?: number;
}

export interface PloyzNatsRequestConnection {
  request(
    subject: string,
    payload?: Uint8Array | string,
    options?: { timeout: number },
  ): Promise<PloyzNatsResponseMessage>;
  close?(): Promise<void>;
  drain?(): Promise<void>;
}

export interface PloyzNatsResponseMessage {
  data: Uint8Array;
  headers?: PloyzNatsResponseHeaders;
}

export interface PloyzNatsResponseHeaders {
  get(name: string): string;
}

export async function connectPloyzNatsTransport(
  options: PloyzNatsConnectOptions = {},
): Promise<PloyzNatsTransport> {
  const connection = await connectNats(options.nats);
  return new PloyzNatsTransport(connection, {
    requestTimeoutMs: options.requestTimeoutMs,
  });
}

export class PloyzNatsTransport {
  readonly #connection: PloyzNatsRequestConnection;
  readonly #requestTimeoutMs: number;

  constructor(
    connection: PloyzNatsRequestConnection,
    options: PloyzNatsTransportOptions = {},
  ) {
    this.#connection = connection;
    this.#requestTimeoutMs = options.requestTimeoutMs ?? DEFAULT_NATS_REQUEST_TIMEOUT_MS;
  }

  deploySubmit(request: DeploySubmitRequest): Promise<DeploySubmitResponse> {
    return this.#request("deploy.submit", request);
  }

  backupCreate(request: BackupCreateRequest): Promise<BackupCreateResponse> {
    return this.#request("backup.create", request);
  }

  machineAdd(request: MachineAddRequest): Promise<MachineAddResponse> {
    return this.#request("machine.add", request);
  }

  machineList(request: MachineListRequest): Promise<MachineListResponse> {
    return this.#request("machine.list", request);
  }

  machineInspect(request: MachineInspectRequest): Promise<MachineInspectResponse> {
    return this.#request("machine.inspect", request);
  }

  machineJoinRedeem(request: MachineJoinRedeemRequest): Promise<MachineJoinRedeemResponse> {
    return this.#request("machine.join.redeem", request);
  }

  machineJoinReport(request: MachineJoinReportRequest): Promise<MachineJoinReportResponse> {
    return this.#request("machine.join.report", request);
  }

  serviceList(request: ServiceListRequest): Promise<ServiceListResponse> {
    return this.#request("service.list", request);
  }

  serviceInspect(request: ServiceInspectRequest): Promise<ServiceInspectResponse> {
    return this.#request("service.inspect", request);
  }

  opsStatus(request: OpsStatusRequest): Promise<OpsStatusResponse> {
    return this.#request("ops.status", request);
  }

  opsWatch(request: OpsWatchRequest): Promise<OpsWatchResponse> {
    return this.#request("ops.watch", request);
  }

  async close(): Promise<void> {
    await this.#connection.close?.();
  }

  async drain(): Promise<void> {
    if (this.#connection.drain) {
      await this.#connection.drain();
      return;
    }
    await this.close();
  }

  async #request<TRequest, TResponse>(
    endpoint: PloyzApiEndpoint,
    request: TRequest,
  ): Promise<TResponse> {
    const subject = operationApiSubject(endpoint);
    let response: PloyzNatsResponseMessage;
    try {
      response = await this.#connection.request(
        subject,
        textEncoder.encode(JSON.stringify(request)),
        { timeout: this.#requestTimeoutMs },
      );
    } catch (error) {
      throw PloyzNatsTransportError.requestFailed(endpoint, error);
    }

    const serviceError = decodeNatsServiceError(endpoint, response.headers);
    if (serviceError) {
      throw PloyzNatsTransportError.serviceError(endpoint, serviceError);
    }

    try {
      return JSON.parse(textDecoder.decode(response.data)) as TResponse;
    } catch (error) {
      throw PloyzNatsTransportError.decodeFailed(endpoint, error);
    }
  }
}

export type PloyzNatsTransportFailure =
  | { kind: "request_failed"; cause: unknown }
  | { kind: "service_error"; code: PloyzNatsServiceErrorCode; message: string }
  | { kind: "service_error_protocol"; message: string }
  | { kind: "decode_response"; cause: unknown };

export class PloyzNatsTransportError extends Error {
  readonly endpoint: PloyzApiEndpoint;
  readonly failure: PloyzNatsTransportFailure;

  private constructor(endpoint: PloyzApiEndpoint, failure: PloyzNatsTransportFailure) {
    super(renderNatsTransportFailure(endpoint, failure));
    this.name = "PloyzNatsTransportError";
    this.endpoint = endpoint;
    this.failure = failure;
  }

  static requestFailed(endpoint: PloyzApiEndpoint, cause: unknown): PloyzNatsTransportError {
    return new PloyzNatsTransportError(endpoint, { kind: "request_failed", cause });
  }

  static serviceError(
    endpoint: PloyzApiEndpoint,
    serviceError: PloyzNatsServiceError,
  ): PloyzNatsTransportError {
    return new PloyzNatsTransportError(endpoint, {
      kind: "service_error",
      code: serviceError.code,
      message: serviceError.message,
    });
  }

  static serviceProtocol(
    endpoint: PloyzApiEndpoint,
    message: string,
  ): PloyzNatsTransportError {
    return new PloyzNatsTransportError(endpoint, {
      kind: "service_error_protocol",
      message,
    });
  }

  static decodeFailed(endpoint: PloyzApiEndpoint, cause: unknown): PloyzNatsTransportError {
    return new PloyzNatsTransportError(endpoint, { kind: "decode_response", cause });
  }
}

export type PloyzNatsServiceErrorCode = 400 | 409 | 500 | 503 | 504;

interface PloyzNatsServiceError {
  code: PloyzNatsServiceErrorCode;
  message: string;
}

function operationApiSubject(endpoint: PloyzApiEndpoint): string {
  const contract = OPERATION_API_CONTRACTS.find((candidate) => candidate.name === endpoint);
  if (!contract) {
    throw new Error(`unknown Ployz API endpoint: ${endpoint}`);
  }

  return contract.subject;
}

function decodeNatsServiceError(
  endpoint: PloyzApiEndpoint,
  headers: PloyzNatsResponseHeaders | undefined,
): PloyzNatsServiceError | undefined {
  const message = headers?.get(NATS_SERVICE_ERROR_HEADER) ?? "";
  const code = headers?.get(NATS_SERVICE_ERROR_CODE_HEADER) ?? "";
  if (message === "" && code === "") {
    return undefined;
  }
  if (message === "") {
    throw PloyzNatsTransportError.serviceProtocol(
      endpoint,
      `${NATS_SERVICE_ERROR_HEADER} is missing`,
    );
  }
  if (code === "") {
    throw PloyzNatsTransportError.serviceProtocol(
      endpoint,
      `${NATS_SERVICE_ERROR_CODE_HEADER} is missing`,
    );
  }

  const parsedCode = Number(code);
  if (!isPloyzNatsServiceErrorCode(parsedCode)) {
    throw PloyzNatsTransportError.serviceProtocol(
      endpoint,
      `${NATS_SERVICE_ERROR_CODE_HEADER} is invalid: ${code}`,
    );
  }

  return { code: parsedCode, message };
}

function isPloyzNatsServiceErrorCode(value: number): value is PloyzNatsServiceErrorCode {
  return value === 400 || value === 409 || value === 500 || value === 503 || value === 504;
}

function renderNatsTransportFailure(
  endpoint: PloyzApiEndpoint,
  failure: PloyzNatsTransportFailure,
): string {
  switch (failure.kind) {
    case "request_failed":
      return `${endpoint} NATS request failed`;
    case "service_error":
      return `${endpoint} NATS service error ${failure.code}: ${failure.message}`;
    case "service_error_protocol":
      return `${endpoint} NATS service error headers are invalid: ${failure.message}`;
    case "decode_response":
      return `${endpoint} NATS response decode failed`;
  }
}
