import assert from "node:assert/strict";
import test from "node:test";

import {
  deploySubmitRequest,
  eventSequence,
  machineJoinToken,
  operationId,
  operationLeaseExpiresAt,
  operationOwnerId,
  nodeId,
  PloyzNatsTransport,
  PloyzNatsTransportError,
} from "../src/index.ts";
import type {
  AcceptedOperation,
  MachineJoinReportRequest,
  PloyzNatsRequestConnection,
  PloyzNatsResponseMessage,
} from "../src/index.ts";

test("NATS transport sends JSON requests to contract subjects", async () => {
  const nats = new RecordingNatsConnection([
    jsonResponse({ status: "ok", value: acceptedOperation("op_123") }),
  ]);
  const transport = new PloyzNatsTransport(nats, { requestTimeoutMs: 1234 });

  const response = await transport.deploySubmit(deploySubmitRequest(deployInput()));

  assert.deepEqual(response, { status: "ok", value: acceptedOperation("op_123") });
  assert.equal(nats.requests[0].subject, "plz.v1.svc.api.deploy.submit");
  const payload = nats.requests[0].payload;
  assert.ok(payload instanceof Uint8Array);
  assert.deepEqual(JSON.parse(new TextDecoder().decode(payload)), {
    operation_id: "op_123",
    idempotency_key: "idem_123",
    target: {
      service_id: "svc_api",
      target_revision: "rev_2",
      image: "ghcr.io/acme/api:rev-2",
      replicas: 1,
    },
  });
  assert.deepEqual(nats.requests[0].options, { timeout: 1234 });
});

test("NATS transport surfaces service error headers before response decoding", async () => {
  const nats = new RecordingNatsConnection([
    {
      data: new TextEncoder().encode("not json"),
      headers: new RecordingHeaders({
        "Nats-Service-Error": "deploy service unavailable",
        "Nats-Service-Error-Code": "503",
      }),
    },
  ]);
  const transport = new PloyzNatsTransport(nats);

  await assert.rejects(
    transport.deploySubmit(deploySubmitRequest(deployInput())),
    (error: unknown) =>
      error instanceof PloyzNatsTransportError &&
      error.endpoint === "deploy.submit" &&
      error.failure.kind === "service_error" &&
      error.failure.code === 503 &&
      error.failure.message === "deploy service unavailable",
  );
});

test("NATS transport rejects invalid service error headers", async () => {
  const nats = new RecordingNatsConnection([
    {
      data: new TextEncoder().encode("{}"),
      headers: new RecordingHeaders({
        "Nats-Service-Error": "broken",
      }),
    },
  ]);
  const transport = new PloyzNatsTransport(nats);

  await assert.rejects(
    transport.opsStatus({ operation_id: operationId("op_123") }),
    (error: unknown) =>
      error instanceof PloyzNatsTransportError &&
      error.endpoint === "ops.status" &&
      error.failure.kind === "service_error_protocol",
  );
});

test("NATS transport covers internal machine join report endpoint", async () => {
  const nats = new RecordingNatsConnection([
    jsonResponse({
      status: "ok",
      value: {
        operation_id: operationId("op_machine"),
        node_id: nodeId("node_2"),
        last_event_sequence: eventSequence(8),
        outcome: { outcome: "completed" },
      },
    }),
  ]);
  const transport = new PloyzNatsTransport(nats);

  const request: MachineJoinReportRequest = {
    join_token: machineJoinToken("join_once_123"),
    outcome: { outcome: "completed" },
  };
  const response = await transport.machineJoinReport(request);

  assert.deepEqual(response, {
    status: "ok",
    value: {
      operation_id: "op_machine",
      node_id: "node_2",
      last_event_sequence: "8",
      outcome: { outcome: "completed" },
    },
  });
  assert.equal(nats.requests[0].subject, "plz.v1.svc.api.machine.join.report");
});

test("NATS transport exposes connection close and drain lifecycle", async () => {
  const nats = new RecordingNatsConnection([]);
  const transport = new PloyzNatsTransport(nats);

  await transport.drain();
  await transport.close();

  assert.deepEqual(nats.lifecycle, ["drain", "close"]);
});

class RecordingNatsConnection implements PloyzNatsRequestConnection {
  readonly responses: PloyzNatsResponseMessage[];
  readonly lifecycle: string[] = [];
  readonly requests: Array<{
    subject: string;
    payload?: Uint8Array | string;
    options?: { timeout: number };
  }> = [];

  constructor(responses: PloyzNatsResponseMessage[]) {
    this.responses = [...responses];
  }

  async request(
    subject: string,
    payload?: Uint8Array | string,
    options?: { timeout: number },
  ): Promise<PloyzNatsResponseMessage> {
    this.requests.push({ subject, payload, options });
    const response = this.responses.shift();
    if (!response) {
      throw new Error("missing fake NATS response");
    }

    return response;
  }

  async close(): Promise<void> {
    this.lifecycle.push("close");
  }

  async drain(): Promise<void> {
    this.lifecycle.push("drain");
  }
}

class RecordingHeaders {
  readonly values: Record<string, string>;

  constructor(values: Record<string, string>) {
    this.values = values;
  }

  get(name: string): string {
    return this.values[name] ?? "";
  }
}

function jsonResponse(value: unknown): PloyzNatsResponseMessage {
  return {
    data: new TextEncoder().encode(JSON.stringify(value)),
  };
}

function deployInput() {
  return {
    operationId: "op_123",
    idempotencyKey: "idem_123",
    serviceId: "svc_api",
    targetRevision: "rev_2",
    image: "ghcr.io/acme/api:rev-2",
    replicas: 1,
  };
}

function acceptedOperation(operationIdValue: string): AcceptedOperation {
  return {
    operation_id: operationId(operationIdValue),
    watch_subject: `plz.v1.op.${operationIdValue}.>`,
    start_sequence: eventSequence(11),
    owner_lease: {
      operation_id: operationId(operationIdValue),
      owner_id: operationOwnerId("control"),
      expires_at: operationLeaseExpiresAt(120),
    },
  };
}
