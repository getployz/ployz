import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  certBundleRef,
  deploySubmitRequest,
  eventSequence,
  machineAddRequest,
  machineBootstrapUrl,
  machineJoinRedeemRequest,
  machineJoinToken,
  machineName,
  MAX_OPERATION_EVENT_REPLAY_LIMIT,
  nodeId,
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
  MachineAddAccepted,
  MachineAddResponse,
  MachineAddRequest,
  MachineJoinBundle,
  MachineJoinRedeemed,
  MachineJoinRedeemResponse,
  MachineJoinRedeemRequest,
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

test("machine add returns an operation handle with bootstrap material", async () => {
  const transport = new RecordingTransport(defaultFixture());
  const client = new PloyzClient(transport);
  const input = machineAddInput();
  const request = machineAddRequest(input);

  const handle = await client.machineAdd(input);
  const status = await handle.status();

  assert.equal(handle.operationId, "op_machine");
  assert.equal(handle.nodeId, "node_2");
  assert.equal(handle.bootstrapUrl, "https://get.ployz.sh");
  assert.equal(handle.joinToken, "join_once_123");
  assert.deepEqual(transport.machineAddRequests, [request]);
  assert.deepEqual(transport.statusRequests, [{ operation_id: "op_machine" }]);
  assert.deepEqual(status, defaultFixture().operation_status_snapshot);
});

test("machine join redeem returns joined machine facts", async () => {
  const transport = new RecordingTransport(defaultFixture());
  const client = new PloyzClient(transport);
  const input = { joinToken: "join_once_123" };
  const request = machineJoinRedeemRequest(input);

  const redeemed = await client.machineJoinRedeem(input);

  assert.deepEqual(transport.machineJoinRedeemRequests, [request]);
  assert.deepEqual(redeemed, defaultFixture().machine_join_redeem_response.value);
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
  assert.deepEqual(transport.machineAddAccepted, fixture.machine_add_response.value);
  assert.deepEqual(transport.status, fixture.operation_status_snapshot);
  assert.deepEqual(transport.replay, fixture.operation_event_replay_page);
  assert.deepEqual(fixture.deploy_submit_response, {
    status: "ok",
    value: fixture.accepted_operation,
  });
  assert.deepEqual(fixture.machine_add_response, {
    status: "ok",
    value: fixture.machine_add_response.value,
  });
  assert.deepEqual(fixture.machine_join_redeem_response, {
    status: "ok",
    value: fixture.machine_join_redeem_response.value,
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

test("sdk maps raw machine add input to the wire request", () => {
  assert.deepEqual(machineAddRequest(machineAddInput()), {
    operation_id: "op_machine",
    idempotency_key: "idem_machine",
    node_id: "node_2",
    name: "edge_2",
    gateway: "skip",
    join_bundle: machineJoinBundle(),
  });
  assert.throws(
    () => machineAddRequest({ ...machineAddInput(), name: "edge.2" }),
    /machine name/,
  );
  assert.throws(
    () => machineAddRequest({ ...machineAddInput(), nodeId: "node.2" }),
    /node id/,
  );
});

test("sdk maps raw machine join redeem input to the wire request", () => {
  assert.deepEqual(machineJoinRedeemRequest({ joinToken: "join_once_123" }), {
    join_token: "join_once_123",
  });
  assert.throws(
    () => machineJoinRedeemRequest({ joinToken: "join token" }),
    /join token/,
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
      name: "machine.add",
      subject: "plz.v1.svc.api.machine.add",
      execution: "accepts_operation",
      request: "MachineAddRequest",
      success: "MachineAddAccepted",
      error: "MachineAddError",
      response: "MachineAddResponse",
    },
    {
      name: "machine.join.redeem",
      subject: "plz.v1.svc.api.machine.join.redeem",
      execution: "mutates_operation",
      request: "MachineJoinRedeemRequest",
      success: "MachineJoinRedeemed",
      error: "MachineJoinRedeemError",
      response: "MachineJoinRedeemResponse",
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
  assert.equal(machineBootstrapUrl("https://get.ployz.sh"), "https://get.ployz.sh");
  assert.throws(() => machineBootstrapUrl("http://get.ployz.sh"));
  assert.equal(machineJoinToken("join_once_123"), "join_once_123");
  assert.throws(() => machineJoinToken("join token"));
});

class RecordingTransport implements PloyzOperationTransport {
  readonly deployRequests: DeploySubmitRequest[] = [];
  readonly machineAddRequests: MachineAddRequest[] = [];
  readonly machineJoinRedeemRequests: MachineJoinRedeemRequest[] = [];
  readonly statusRequests: OpsStatusRequest[] = [];
  readonly watchRequests: OpsWatchRequest[] = [];
  readonly accepted: AcceptedOperation;
  readonly machineAddAccepted: MachineAddAccepted;
  readonly machineJoinRedeemed: MachineJoinRedeemed;
  readonly status: OperationStatusSnapshot;
  readonly replay: OperationEventReplayPage;
  readonly replayPages: OperationEventReplayPage[] = [];
  deployResponse?: DeploySubmitResponse;
  machineAddResponse?: MachineAddResponse;
  machineJoinRedeemResponse?: MachineJoinRedeemResponse;
  statusResponse?: OpsStatusResponse;
  watchResponse?: OpsWatchResponse;

  constructor(fixture: OperationFixture) {
    this.accepted = fixture.accepted_operation;
    this.machineAddAccepted = fixture.machine_add_response.value;
    this.machineJoinRedeemed = fixture.machine_join_redeem_response.value;
    this.status = fixture.operation_status_snapshot;
    this.replay = fixture.operation_event_replay_page;
  }

  async deploySubmit(request: DeploySubmitRequest): Promise<DeploySubmitResponse> {
    this.deployRequests.push(request);
    return this.deployResponse ?? { status: "ok", value: this.accepted };
  }

  async machineAdd(request: MachineAddRequest): Promise<MachineAddResponse> {
    this.machineAddRequests.push(request);
    return this.machineAddResponse ?? { status: "ok", value: this.machineAddAccepted };
  }

  async machineJoinRedeem(
    request: MachineJoinRedeemRequest,
  ): Promise<MachineJoinRedeemResponse> {
    this.machineJoinRedeemRequests.push(request);
    return this.machineJoinRedeemResponse ?? { status: "ok", value: this.machineJoinRedeemed };
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
  machine_add_response: { status: "ok"; value: MachineAddAccepted };
  machine_join_redeem_response: { status: "ok"; value: MachineJoinRedeemed };
  operation_status_snapshot: OperationStatusSnapshot;
  operation_event_replay_page: OperationEventReplayPage;
}

function defaultFixture(): OperationFixture {
  return {
    accepted_operation: acceptedOperation("op_123"),
    machine_add_response: {
      status: "ok",
      value: {
        accepted: {
          ...acceptedOperation("op_machine"),
          start_sequence: eventSequence(7),
        },
        node_id: nodeId("node_2"),
        bootstrap_url: machineBootstrapUrl("https://get.ployz.sh"),
        join_token: machineJoinToken("join_once_123"),
      },
    },
    machine_join_redeem_response: {
      status: "ok",
      value: {
        operation_id: operationId("op_machine"),
        node_id: nodeId("node_2"),
        name: machineName("edge_2"),
        gateway: "skip",
        join_bundle: machineJoinBundle(),
        joined_at: "60" as MachineJoinRedeemed["joined_at"],
        last_event_sequence: eventSequence(8),
        result: "joined",
      },
    },
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

function machineAddInput() {
  return {
    operationId: "op_machine",
    idempotencyKey: "idem_machine",
    nodeId: "node_2",
    name: "edge_2",
    gateway: "skip" as const,
    joinBundle: machineJoinBundle(),
  };
}

function machineJoinBundle(): MachineJoinBundle {
  return {
    cluster_name: "prod",
    ployzd: {
      version: "0.1.0",
      source: "/tmp/ployzd",
      sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      install_path: "/usr/local/bin/ployzd",
    },
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
