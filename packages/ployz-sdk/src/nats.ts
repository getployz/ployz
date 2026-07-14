import {
  connect as connectNats,
  type NodeConnectionOptions,
} from "@nats-io/transport-node";

import { OPERATION_API_CONTRACTS } from "./generated.ts";
import type {
  OperationApiRequestByEndpoint,
  OperationApiResponseByEndpoint,
  PloyzApiEndpoint,
  RuntimeSnapshot,
} from "./generated.ts";

const DEFAULT_NATS_REQUEST_TIMEOUT_MS = 10_000;
const NATS_SERVICE_ERROR_HEADER = "Nats-Service-Error";
const NATS_SERVICE_ERROR_CODE_HEADER = "Nats-Service-Error-Code";
const RUNTIME_SNAPSHOT_PROJECTION_SUBJECT = "plz.v1.projection.runtime.snapshot";

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

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
  subscribe(subject: string): PloyzNatsSubscription;
  status(): AsyncIterable<PloyzNatsStatus>;
  closed(): Promise<void | Error>;
  close?(): Promise<void>;
  drain?(): Promise<void>;
}

export interface PloyzNatsMessage {
  data: Uint8Array;
}

export interface PloyzNatsSubscription extends AsyncIterable<PloyzNatsMessage> {
  unsubscribe(): void;
}

export interface PloyzNatsStatus {
  type: string;
  error?: Error;
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

  async request<E extends PloyzApiEndpoint>(
    endpoint: E,
    request: OperationApiRequestByEndpoint[E],
  ): Promise<OperationApiResponseByEndpoint[E]> {
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
      return JSON.parse(textDecoder.decode(response.data)) as OperationApiResponseByEndpoint[E];
    } catch (error) {
      throw PloyzNatsTransportError.decodeFailed(endpoint, error);
    }
  }

  async *watchRuntime(): AsyncIterable<RuntimeSnapshot> {
    const subscription = this.#connection.subscribe(RUNTIME_SNAPSHOT_PROJECTION_SUBJECT);
    const statusIterator = this.#connection.status()[Symbol.asyncIterator]();
    let latest: RuntimeSnapshot | undefined;
    const terminal: { value?: { error?: unknown } } = {};
    let cancelled = false;
    let wake: (() => void) | undefined;

    const signal = (): void => {
      wake?.();
      wake = undefined;
    };
    const publish = (snapshot: RuntimeSnapshot): void => {
      if (cancelled || terminal.value) return;
      latest = snapshot;
      signal();
    };
    const finish = (error?: unknown): void => {
      if (cancelled || terminal.value) return;
      terminal.value = { error };
      signal();
    };
    const seed = async (): Promise<RuntimeSnapshot> => {
      const response = await this.request("runtime.snapshot", {});
      if (response.status === "domain_error") {
        throw new Error("runtime.snapshot returned a domain error");
      }
      return response.value.snapshot;
    };

    void (async () => {
      try {
        for await (const message of subscription) {
          try {
            publish(JSON.parse(textDecoder.decode(message.data)) as RuntimeSnapshot);
          } catch (error) {
            finish(PloyzNatsTransportError.decodeFailed("runtime.snapshot", error));
          }
        }
        finish();
      } catch (error) {
        finish(error);
      }
    })();
    void (async () => {
      try {
        while (true) {
          const status = await statusIterator.next();
          if (status.done) return;
          if (status.value.type === "reconnect") {
            try {
              publish(await seed());
            } catch (error) {
              if (
                error instanceof PloyzNatsTransportError &&
                error.failure.kind === "request_failed"
              ) {
                continue;
              }
              throw error;
            }
          }
          if (status.value.type === "error") finish(status.value.error);
        }
      } catch (error) {
        finish(error);
      }
    })();
    void this.#connection.closed().then((error) => {
      if (error instanceof Error) finish(error);
      else finish();
    }, finish);

    try {
      const initial = await seed();
      if (terminal.value?.error !== undefined) throw terminal.value.error;
      if (terminal.value) return;
      yield initial;

      while (true) {
        if (latest !== undefined) {
          const value = latest;
          latest = undefined;
          yield value;
          continue;
        }
        const outcome = terminal.value as { error?: unknown } | undefined;
        if (outcome?.error !== undefined) throw outcome.error;
        if (outcome) return;
        await new Promise<void>((resolve) => {
          wake = resolve;
        });
      }
    } finally {
      cancelled = true;
      subscription.unsubscribe();
      await statusIterator.return?.();
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
