import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  certBundleRef,
  deploySubmitRequest,
  eventSequence,
  MAX_OPERATION_EVENT_REPLAY_LIMIT,
  OPERATION_API_CONTRACTS,
  operationId,
  operationEventReplayLimit,
  operationLeaseExpiresAt,
  operationOwnerId,
  PloyzApiError,
  PloyzClient,
  serviceId,
} from "../src/index.ts";
import type {
  AcceptedOperation,
  DeploySubmitResponse,
  DeploySubmitRequest,
  OperationEventReplayPage,
  OperationStatusSnapshot,
  OperationSubject,
  OpsStatusResponse,
  OpsStatusRequest,
  OpsWatchResponse,
  OpsWatchRequest,
  PloyzOperationTransport,
} from "../src/index.ts";

const fixturePath = join(
  dirname(fileURLToPath(import.meta.url)),
  "fixtures",
  "operation-contract.json",
);

test("deploy returns an operation handle with status and replay helpers", async () => {
  const transport = new RecordingTransport(defaultFixture());
  const client = new PloyzClient(transport);
  const input = deployInput();
  const request = deploySubmitRequest(input);

  const handle = await client.deploy(input);
  const status = await handle.status();
  const page = await handle.replayFromStart(100);

  assert.equal(handle.operationId, "op_123");
  assert.deepEqual(transport.deployRequests, [request]);
  assert.deepEqual(transport.statusRequests, [{ operation_id: "op_123" }]);
  assert.deepEqual(transport.watchRequests, [
    { operation_id: "op_123", start_sequence: "11", limit: 100 },
  ]);
  assert.deepEqual(status, defaultFixture().operation_status_snapshot);
  assert.deepEqual(page, defaultFixture().operation_event_replay_page);
});

test("replay pages advance through replay cursors", async () => {
  const transport = new RecordingTransport(defaultFixture());
  transport.replayPages.push(
    {
      events: [],
      cursor: { state: "more", next_start_sequence: eventSequence(12) },
    },
    {
      events: [],
      cursor: { state: "terminal" },
    },
  );
  const client = new PloyzClient(transport);
  const handle = await client.deploy(deployInput());

  const pages: OperationEventReplayPage[] = [];
  for await (const page of handle.replayPages(50)) {
    pages.push(page);
  }

  assert.deepEqual(transport.watchRequests, [
    { operation_id: "op_123", start_sequence: "11", limit: 50 },
    { operation_id: "op_123", start_sequence: "12", limit: 50 },
  ]);
  assert.deepEqual(
    pages.map((page) => page.cursor),
    [{ state: "more", next_start_sequence: "12" }, { state: "terminal" }],
  );
});

test("domain errors are decoded once by the client", async () => {
  const transport = new RecordingTransport(defaultFixture());
  transport.deployResponse = {
    status: "domain_error",
    error: {
      error: "duplicate_sequence_mismatch",
      operation_id: operationId("op_123"),
      sequence: eventSequence(2),
    },
  };
  const client = new PloyzClient(transport);

  await assert.rejects(
    client.deploy(deployInput()),
    (error: unknown) =>
      error instanceof PloyzApiError &&
      error.endpoint === "deploy.submit" &&
      error.error.error === "duplicate_sequence_mismatch",
  );
});

test("accepted operation uses Rust wire field names", () => {
  const accepted = acceptedOperation("op_123");

  assert.deepEqual(JSON.parse(JSON.stringify(accepted)), {
    operation_id: "op_123",
    watch_subject: "plz.v1.op.op_123.>",
    start_sequence: "11",
    owner_lease: {
      operation_id: "op_123",
      owner_id: "control",
      expires_at: "120",
    },
  });
});

test("TypeScript DTOs match the Rust-emitted operation fixture", () => {
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8"));
  const transport = new RecordingTransport(fixture);

  assert.deepEqual(transport.accepted, fixture.accepted_operation);
  assert.deepEqual(transport.status, fixture.operation_status_snapshot);
  assert.deepEqual(transport.replay, fixture.operation_event_replay_page);
  assert.deepEqual(fixture.deploy_submit_response, {
    status: "ok",
    value: fixture.accepted_operation,
  });
  assert.deepEqual(fixture.ops_status_response, {
    status: "ok",
    value: fixture.operation_status_snapshot,
  });
  assert.deepEqual(fixture.ops_watch_response, {
    status: "ok",
    value: fixture.operation_event_replay_page,
  });
});

test("sdk does not expose node service calls", () => {
  const names = new Set(Reflect.ownKeys(PloyzClient.prototype));

  assert.equal(names.has("nodeService"), false);
  assert.equal(names.has("containerRun"), false);
});

test("sdk maps raw deploy input to the wire request", () => {
  assert.deepEqual(deploySubmitRequest(deployInput()), {
    operation_id: "op_123",
    idempotency_key: "idem_123",
    target: {
      service_id: "svc_api",
      target_revision: "rev_2",
      image: "ghcr.io/acme/api:rev-2",
      replicas: 1,
    },
  });
  assert.throws(
    () => deploySubmitRequest({ ...deployInput(), serviceId: "svc.api" }),
    /service id/,
  );
  assert.throws(
    () => deploySubmitRequest({ ...deployInput(), replicas: 0 }),
    /replica count/,
  );
});

test("sdk exports operation subjects", () => {
  const subject: OperationSubject = {
    kind: "deploy",
    service_id: serviceId("svc_api"),
  };

  assert.deepEqual(subject, { kind: "deploy", service_id: "svc_api" });
});

test("sdk exports the Rust operation API contract registry", () => {
  assert.deepEqual(OPERATION_API_CONTRACTS, [
    {
      name: "deploy.submit",
      subject: "plz.v1.svc.api.deploy.submit",
      execution: "accepts_operation",
      request: "DeploySubmitRequest",
      success: "AcceptedOperation",
      error: "DeploySubmitError",
      response: "DeploySubmitResponse",
    },
    {
      name: "ops.status",
      subject: "plz.v1.svc.api.ops.status",
      execution: "query",
      request: "OpsStatusRequest",
      success: "OperationStatusSnapshot",
      error: "OpsStatusError",
      response: "OpsStatusResponse",
    },
    {
      name: "ops.watch",
      subject: "plz.v1.svc.api.ops.watch",
      execution: "query",
      request: "OpsWatchRequest",
      success: "OperationEventReplayPage",
      error: "OpsWatchError",
      response: "OpsWatchResponse",
    },
  ]);
});

test("sdk helpers enforce public primitive boundaries", () => {
  assert.equal(operationEventReplayLimit(MAX_OPERATION_EVENT_REPLAY_LIMIT), 512);
  assert.throws(() => operationEventReplayLimit(MAX_OPERATION_EVENT_REPLAY_LIMIT + 1));
  assert.equal(eventSequence("18446744073709551615"), "18446744073709551615");
  assert.equal(eventSequence(12n), "12");
  assert.throws(() => eventSequence(Number.MAX_SAFE_INTEGER + 1));
  assert.throws(() => eventSequence("18446744073709551616"));
  assert.equal(certBundleRef("obj://PLZ_CERTS/cert_api/rev_1"), "obj://PLZ_CERTS/cert_api/rev_1");
  assert.throws(() => certBundleRef("obj://PLZ_CERTS//rev_1"));
});

class RecordingTransport implements PloyzOperationTransport {
  readonly deployRequests: DeploySubmitRequest[] = [];
  readonly statusRequests: OpsStatusRequest[] = [];
  readonly watchRequests: OpsWatchRequest[] = [];
  readonly accepted: AcceptedOperation;
  readonly status: OperationStatusSnapshot;
  readonly replay: OperationEventReplayPage;
  readonly replayPages: OperationEventReplayPage[] = [];
  deployResponse?: DeploySubmitResponse;
  statusResponse?: OpsStatusResponse;
  watchResponse?: OpsWatchResponse;

  constructor(fixture: OperationFixture) {
    this.accepted = fixture.accepted_operation;
    this.status = fixture.operation_status_snapshot;
    this.replay = fixture.operation_event_replay_page;
  }

  async deploySubmit(request: DeploySubmitRequest): Promise<DeploySubmitResponse> {
    this.deployRequests.push(request);
    return this.deployResponse ?? { status: "ok", value: this.accepted };
  }

  async opsStatus(request: OpsStatusRequest): Promise<OpsStatusResponse> {
    this.statusRequests.push(request);
    return this.statusResponse ?? { status: "ok", value: this.status };
  }

  async opsWatch(request: OpsWatchRequest): Promise<OpsWatchResponse> {
    this.watchRequests.push(request);
    if (this.watchResponse) {
      return this.watchResponse;
    }

    return { status: "ok", value: this.replayPages.shift() ?? this.replay };
  }
}

interface OperationFixture {
  accepted_operation: AcceptedOperation;
  operation_status_snapshot: OperationStatusSnapshot;
  operation_event_replay_page: OperationEventReplayPage;
}

function defaultFixture(): OperationFixture {
  return {
    accepted_operation: acceptedOperation("op_123"),
    operation_status_snapshot: {
      status: {
        kind: "deploy",
        id: operationId("op_123"),
        service_id: serviceId("svc_api"),
        state: { state: "accepted" },
        last_event_sequence: eventSequence(11),
      },
      ownership: {
        state: "owned",
        lease: {
          operation_id: operationId("op_123"),
          owner_id: operationOwnerId("control"),
          expires_at: operationLeaseExpiresAt(120),
        },
      },
    },
    operation_event_replay_page: {
      events: [],
      cursor: { state: "caught_up" },
    },
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
