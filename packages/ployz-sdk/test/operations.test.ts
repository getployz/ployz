import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  certBundleRef,
  containerId,
  deployReservationId,
  deployReserveRequest,
  deploySubmitRequest,
  eventSequence,
  imageReference,
  initFirstMachineActivateRequest,
  logsTailRequest,
  logsTailLines,
  machineInspectRequest,
  machineAddRequest,
  machineUpdateRequest,
  machineBootstrapUrl,
  machineJoinRedeemRequest,
  machineListRequest,
  machineJoinToken,
  machineName,
  MAX_OPERATION_EVENT_REPLAY_LIMIT,
  machineId,
  namespaceId,
  OPERATION_API_CONTRACTS,
  operationId,
  operationEventReplayLimit,
  opsListRequest,
  PloyzApiError,
  PloyzClient,
  namespaceRevisionEntryId,
  replicaCount,
  runtimeSnapshotRequest,
  serviceInspectRequest,
  serviceId,
  serviceListRequest,
} from "../src/index.ts";
import type {
  AcceptedOperation,
  DeployReserveRequest,
  DeployReserveResponse,
  DeployReserved,
  DeploySubmitResponse,
  DeploySubmitRequest,
  InitFirstMachineActivateRequest,
  InitFirstMachineActivateResponse,
  InitFirstMachineActivated,
  LogsTailResponse,
  LogsTailRequest,
  LogsTailResult,
  MachineAddAccepted,
  MachineAddResponse,
  MachineAddRequest,
  MachineInspectResponse,
  MachineInspectRequest,
  MachineJoinBundle,
  MachineJoinSecretDelivery,
  MachineJoinRedeemed,
  MachineJoinRedeemResponse,
  MachineJoinRedeemRequest,
  MachineListResponse,
  MachineListRequest,
  MachineSnapshot,
  MachineUpdateRequest,
  MachineUpdateResponse,
  OperationEventReplayPage,
  OperationStatusSnapshot,
  OperationSubject,
  OpsListRequest,
  OpsListResult,
  OpsStatusResponse,
  OpsStatusRequest,
  OpsWatchResponse,
  OpsWatchRequest,
  PloyzOperationTransport,
  PloyzApiEndpoint,
  RuntimeSnapshot,
  RuntimeSnapshotRequest,
  RuntimeSnapshotResponse,
  ServiceInspectResponse,
  ServiceInspectRequest,
  ServiceListResponse,
  ServiceListRequest,
  ServiceSnapshot,
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
  const reservationId = deployReservationId(1);
  const request = deploySubmitRequest(input, reservationId);

  const handle = await client.deploy(input);
  const status = await handle.status();
  const page = await handle.replayFromStart(100);

  assert.equal(handle.operationId, "op_123");
  assert.deepEqual(transport.deployReserveRequests, [deployReserveRequest(input)]);
  assert.deepEqual(transport.deployRequests, [request]);
  assert.deepEqual(transport.requestEndpoints.slice(0, 2), ["deploy.reserve", "deploy.submit"]);
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
  assert.equal(handle.machineId, "machine_2");
  assert.equal(handle.bootstrapUrl, "https://get.ployz.sh");
  assert.deepEqual(handle.joinBundle, machineJoinBundle());
  assert.equal(handle.runtimeNatsUrl, "nats://127.0.0.1:7422");
  assert.equal(handle.joinToken, "join_once_123");
  assert.deepEqual(transport.machineAddRequests, [request]);
  assert.deepEqual(transport.statusRequests, [{ operation_id: "op_machine" }]);
  assert.deepEqual(status, defaultFixture().operation_status_snapshot);
});

test("machine update returns an operation handle", async () => {
  const transport = new RecordingTransport(defaultFixture());
  const client = new PloyzClient(transport);
  const input = machineUpdateInput();
  const request = machineUpdateRequest(input);

  const handle = await client.machineUpdate(input);
  const status = await handle.status();

  assert.equal(handle.operationId, "op_123");
  assert.deepEqual(transport.machineUpdateRequests, [request]);
  assert.deepEqual(transport.statusRequests, [{ operation_id: "op_123" }]);
  assert.deepEqual(status, defaultFixture().operation_status_snapshot);
});

test("first-machine activation returns a status-capable operation reference", async () => {
  const transport = new RecordingTransport(defaultFixture());
  const client = new PloyzClient(transport);
  const input = { machineId: "core_1", roles: gatewaySkippedRoles() };
  const request = initFirstMachineActivateRequest(input);

  const handle = await client.initFirstMachineActivate(input);
  const status = await handle.status();

  assert.equal(handle.operationId, "op_init_core_1");
  assert.equal(handle.machineId, "core_1");
  assert.deepEqual(transport.initFirstMachineActivateRequests, [request]);
  assert.deepEqual(transport.statusRequests, [{ operation_id: "op_init_core_1" }]);
  assert.deepEqual(status, defaultFixture().operation_status_snapshot);
});

test("machine queries return current state snapshots", async () => {
  const fixture = defaultFixture();
  const transport = new RecordingTransport(fixture);
  const client = new PloyzClient(transport);

  const machines = await client.machineList();
  const machine = await client.machineInspect("machine_2");

  assert.deepEqual(transport.machineListRequests, [machineListRequest()]);
  assert.deepEqual(transport.machineInspectRequests, [machineInspectRequest("machine_2")]);
  assert.deepEqual(machines, fixture.machine_snapshots);
  assert.deepEqual(machine, fixture.machine_snapshots[0]);
});

test("service queries return current active service snapshots", async () => {
  const fixture = defaultFixture();
  const transport = new RecordingTransport(fixture);
  const client = new PloyzClient(transport);

  const services = await client.serviceList();
  const service = await client.serviceInspect({ serviceId: "svc_api" });

  assert.deepEqual(transport.serviceListRequests, [serviceListRequest()]);
  assert.deepEqual(transport.serviceInspectRequests, [
    serviceInspectRequest({ serviceId: "svc_api" }),
  ]);
  assert.deepEqual(services, fixture.service_snapshots);
  assert.deepEqual(service, fixture.service_snapshots[0]);
});

test("runtime snapshot returns Rust-owned projection", async () => {
  const fixture = defaultFixture();
  const transport = new RecordingTransport(fixture);
  const client = new PloyzClient(transport);

  const snapshot = await client.runtimeSnapshot();

  assert.deepEqual(transport.runtimeSnapshotRequests, [runtimeSnapshotRequest()]);
  assert.deepEqual(snapshot, fixture.runtime_snapshot);
});

test("logs tail returns recent container evidence", async () => {
  const transport = new RecordingTransport(defaultFixture());
  const client = new PloyzClient(transport);
  const input = { containerId: "ctr_failed", machineId: "machine_a", tailLines: 25 };
  const result: LogsTailResult = await client.logsTail(input);

  assert.deepEqual(transport.logsTailRequests, [logsTailRequest(input)]);
  assert.deepEqual(result, {
    target: {
      target: "container",
      machine_id: machineId("machine_a"),
      container_id: containerId("ctr_failed"),
    },
    text: "hello\n",
    truncated: false,
  });
});

test("ops list returns operation snapshots", async () => {
  const transport = new RecordingTransport(defaultFixture());
  const client = new PloyzClient(transport);
  const result: OpsListResult = await client.opsList({ activeOnly: true });

  assert.deepEqual(result, { operations: [] });
  assert.deepEqual(transport.opsListRequests, [opsListRequest({ activeOnly: true })]);
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
    watch_subject: "plz.v1.progress.namespace.default.operation.op_123.>",
    start_sequence: "11",
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

test("sdk does not expose machine service calls", () => {
  const names = new Set(Reflect.ownKeys(PloyzClient.prototype));

  assert.equal(names.has("nodeService"), false);
  assert.equal(names.has("containerRun"), false);
});

test("sdk maps raw deploy input to the wire request", () => {
  const reservationId = deployReservationId(1);
  assert.deepEqual(deploySubmitRequest(deployInput(), reservationId), {
    idempotency_key: "idem_deploy_123",
    reservation_id: "1",
    target: {
      namespace_id: "default",
      services: [
        {
          service_id: "svc_api",
          image: "ghcr.io/acme/api:rev-2",
          replicas: 1,
          runtime: {
            command: null,
            entrypoint: null,
            environment: {},
            stop_grace_period: 10,
            volume_mounts: [],
          },
          routes: [],
        },
      ],
    },
  });
  assert.deepEqual(
    deploySubmitRequest(
      {
        ...deployInput(),
        routes: [{ hostname: "api.example.com", port: 443, endpointPort: 8080 }],
      },
      reservationId,
    ),
    {
      idempotency_key: "idem_deploy_123",
      reservation_id: "1",
      target: {
        namespace_id: "default",
        services: [
          {
            service_id: "svc_api",
            image: "ghcr.io/acme/api:rev-2",
            replicas: 1,
            runtime: {
            command: null,
            entrypoint: null,
            environment: {},
            stop_grace_period: 10,
            volume_mounts: [],
          },
            routes: [
              {
                target: {
                  kind: "hostname",
                  hostname: "api.example.com",
                  port: 443,
                },
                endpoint_port: 8080,
              },
            ],
          },
        ],
      },
    },
  );
  assert.throws(
    () => deploySubmitRequest({ ...deployInput(), serviceId: "svc.api" }, reservationId),
    /service id/,
  );
  assert.throws(
    () => deploySubmitRequest({ ...deployInput(), replicas: 0 }, reservationId),
    /replica count/,
  );
});

test("sdk maps raw machine add input to the wire request", () => {
  assert.deepEqual(machineAddRequest(machineAddInput()), {
    operation_id: "op_machine",
    idempotency_key: "idem_machine",
    machine_id: "machine_2",
    name: "edge_2",
    roles: gatewaySkippedRoles(),
  });
  assert.throws(
    () => machineAddRequest({ ...machineAddInput(), name: "edge.2" }),
    /machine name/,
  );
  assert.throws(
    () => machineAddRequest({ ...machineAddInput(), machineId: "machine.2" }),
    /machine id/,
  );
});

test("sdk emits the exact required role wire shape", () => {
  const roles = { gateway: "skip" as const, dns: "skip" as const };

  assert.deepEqual(machineAddRequest({ ...machineAddInput(), roles }).roles, {
    gateway: "skip",
  });
  assert.deepEqual(initFirstMachineActivateRequest({ machineId: "core_1", roles }).roles, {
    gateway: "skip",
  });
});

test("sdk maps raw machine update input to the wire request", () => {
  assert.deepEqual(machineUpdateRequest(machineUpdateInput()), {
    operation_id: "op_update_machine_2_123",
    machine_id: "machine_2",
    target_version: "0.2.0",
  });
  assert.throws(
    () => machineUpdateRequest({ ...machineUpdateInput(), machineId: "machine.2" }),
    /machine id/,
  );
  assert.throws(
    () => machineUpdateRequest({ ...machineUpdateInput(), targetVersion: "0.2.0 beta" }),
    /install artifact version/,
  );
});

test("sdk maps current-state query inputs to wire requests", () => {
  assert.deepEqual(machineListRequest(), {});
  assert.deepEqual(machineInspectRequest({ machineId: "machine_2" }), { machine_id: "machine_2" });
  assert.deepEqual(machineInspectRequest("machine_2"), { machine_id: "machine_2" });
  assert.deepEqual(serviceListRequest(), {});
  assert.deepEqual(serviceInspectRequest({ serviceId: "svc_api" }), {
    namespace_id: "default",
    service_id: "svc_api",
  });
  assert.deepEqual(serviceInspectRequest("svc_api"), {
    namespace_id: "default",
    service_id: "svc_api",
  });
  assert.deepEqual(runtimeSnapshotRequest(), {});
  assert.deepEqual(opsListRequest(), { active_only: false });
  assert.deepEqual(opsListRequest({ activeOnly: true }), { active_only: true });
  assert.deepEqual(opsListRequest({ activeOnly: false }), { active_only: false });
  assert.deepEqual(logsTailRequest("ctr_failed"), {
    target: { target: "container", container_id: "ctr_failed" },
  });
  assert.deepEqual(logsTailRequest({ containerId: "ctr_failed", tailLines: 25 }), {
    target: { target: "container", container_id: "ctr_failed" },
    tail_lines: logsTailLines(25),
  });
  assert.deepEqual(
    logsTailRequest({ containerId: "ctr_failed", machineId: "machine_a", tailLines: 25 }),
    {
      target: {
        target: "container",
        container_id: "ctr_failed",
        machine_id: "machine_a",
      },
      tail_lines: logsTailLines(25),
    },
  );
  assert.throws(() => machineInspectRequest("machine.2"), /machine id/);
  assert.throws(() => serviceInspectRequest("svc.api"), /service id/);
  assert.throws(() => logsTailRequest({ containerId: "ctr.failed" }), /container id/);
  assert.throws(() => logsTailRequest({ containerId: "ctr_failed", tailLines: 1001 }), /logs tail/);
});

test("sdk maps raw first-machine activation input to the wire request", () => {
  assert.deepEqual(initFirstMachineActivateRequest({ machineId: "core_1", roles: installAllRoles() }), {
    machine_id: "core_1",
    roles: installAllRoles(),
    public_url_mode: "auto",
  });
  assert.deepEqual(
    initFirstMachineActivateRequest({
      machineId: "core_1",
      roles: installAllRoles(),
      publicUrlMode: "none",
    }),
    {
      machine_id: "core_1",
      roles: installAllRoles(),
      public_url_mode: "none",
    },
  );
  assert.throws(
    () => initFirstMachineActivateRequest({ machineId: "core.1", roles: gatewaySkippedRoles() }),
    /machine id/,
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
      name: "deploy.reserve",
      subject: "plz.v1.rpc.operator.command.deploy.reserve",
      execution: "mutates_operation",
      request: "DeployReserveRequest",
      success: "DeployReserved",
      error: "DeployReserveError",
      response: "DeployReserveResponse",
    },
    {
      name: "deploy.submit",
      subject: "plz.v1.rpc.operator.command.deploy.submit",
      execution: "accepts_operation",
      request: "DeploySubmitRequest",
      success: "AcceptedOperation",
      error: "DeploySubmitError",
      response: "DeploySubmitResponse",
    },
    {
      name: "init.first_machine.activate",
      subject: "plz.v1.rpc.operator.command.init.first_machine.activate",
      execution: "mutates_operation",
      request: "InitFirstMachineActivateRequest",
      success: "InitFirstMachineActivated",
      error: "InitFirstMachineActivateError",
      response: "InitFirstMachineActivateResponse",
    },
    {
      name: "machine.add",
      subject: "plz.v1.rpc.operator.command.machine.add",
      execution: "accepts_operation",
      request: "MachineAddRequest",
      success: "MachineAddAccepted",
      error: "MachineAddError",
      response: "MachineAddResponse",
    },
    {
      name: "machine.update",
      subject: "plz.v1.rpc.operator.command.machine.update",
      execution: "accepts_operation",
      request: "MachineUpdateRequest",
      success: "AcceptedOperation",
      error: "MachineUpdateError",
      response: "MachineUpdateResponse",
    },
    {
      name: "machine.drain",
      subject: "plz.v1.rpc.operator.command.machine.drain",
      execution: "accepts_operation",
      request: "MachineLifecycleRequest",
      success: "AcceptedOperation",
      error: "MachineLifecycleError",
      response: "MachineDrainResponse",
    },
    {
      name: "machine.resume",
      subject: "plz.v1.rpc.operator.command.machine.resume",
      execution: "accepts_operation",
      request: "MachineLifecycleRequest",
      success: "AcceptedOperation",
      error: "MachineLifecycleError",
      response: "MachineResumeResponse",
    },
    {
      name: "service.restart",
      subject: "plz.v1.rpc.operator.command.service.restart",
      execution: "accepts_operation",
      request: "ServiceRestartRequest",
      success: "AcceptedOperation",
      error: "ServiceRestartError",
      response: "ServiceRestartResponse",
    },
    {
      name: "namespace.remove",
      subject: "plz.v1.rpc.operator.command.namespace.remove",
      execution: "accepts_operation",
      request: "NamespaceRemoveRequest",
      success: "AcceptedOperation",
      error: "NamespaceRemoveError",
      response: "NamespaceRemoveResponse",
    },
    {
      name: "volume.remove",
      subject: "plz.v1.rpc.operator.command.volume.remove",
      execution: "accepts_operation",
      request: "VolumeRemoveRequest",
      success: "AcceptedOperation",
      error: "VolumeRemoveError",
      response: "VolumeRemoveResponse",
    },
    {
      name: "core.replace",
      subject: "plz.v1.rpc.operator.command.core.replace",
      execution: "accepts_operation",
      request: "CoreReplaceRequest",
      success: "AcceptedOperation",
      error: "CoreReplaceError",
      response: "CoreReplaceResponse",
    },
    {
      name: "core.replace.report",
      subject: "plz.v1.rpc.operator.command.core.replace.report",
      execution: "mutates_operation",
      request: "CoreReplaceReportRequest",
      success: "CoreReplaceReported",
      error: "CoreReplaceReportError",
      response: "CoreReplaceReportResponse",
    },
    {
      name: "machine.list",
      subject: "plz.v1.rpc.operator.query.machine.list",
      execution: "query",
      request: "MachineListRequest",
      success: "MachineListResult",
      error: "MachineListError",
      response: "MachineListResponse",
    },
    {
      name: "machine.inspect",
      subject: "plz.v1.rpc.operator.query.machine.inspect",
      execution: "query",
      request: "MachineInspectRequest",
      success: "MachineSnapshot",
      error: "MachineInspectError",
      response: "MachineInspectResponse",
    },
    {
      name: "machine.redeem",
      subject: "plz.v1.rpc.join.command.machine.redeem",
      execution: "mutates_operation",
      request: "MachineJoinRedeemRequest",
      success: "MachineJoinRedeemed",
      error: "MachineJoinRedeemError",
      response: "MachineJoinRedeemResponse",
    },
    {
      name: "machine.report",
      subject: "plz.v1.rpc.join.command.machine.report",
      execution: "mutates_operation",
      request: "MachineJoinReportRequest",
      success: "MachineJoinReported",
      error: "MachineJoinReportError",
      response: "MachineJoinReportResponse",
    },
    {
      name: "service.list",
      subject: "plz.v1.rpc.operator.query.service.list",
      execution: "query",
      request: "ServiceListRequest",
      success: "ServiceListResult",
      error: "ServiceListError",
      response: "ServiceListResponse",
    },
    {
      name: "volume.list",
      subject: "plz.v1.rpc.operator.query.volume.list",
      execution: "query",
      request: "VolumeListRequest",
      success: "VolumeListResult",
      error: "VolumeListError",
      response: "VolumeListResponse",
    },
    {
      name: "service.inspect",
      subject: "plz.v1.rpc.operator.query.service.inspect",
      execution: "query",
      request: "ServiceInspectRequest",
      success: "ServiceSnapshot",
      error: "ServiceInspectError",
      response: "ServiceInspectResponse",
    },
    {
      name: "runtime.snapshot",
      subject: "plz.v1.rpc.operator.query.runtime.snapshot",
      execution: "query",
      request: "RuntimeSnapshotRequest",
      success: "RuntimeSnapshotResult",
      error: "RuntimeSnapshotError",
      response: "RuntimeSnapshotResponse",
    },
    {
      name: "logs.tail",
      subject: "plz.v1.rpc.operator.query.logs.tail",
      execution: "query",
      request: "LogsTailRequest",
      success: "LogsTailResult",
      error: "LogsTailError",
      response: "LogsTailResponse",
    },
    {
      name: "ops.list",
      subject: "plz.v1.rpc.operator.query.ops.list",
      execution: "query",
      request: "OpsListRequest",
      success: "OpsListResult",
      error: "OpsListError",
      response: "OpsListResponse",
    },
    {
      name: "ops.status",
      subject: "plz.v1.rpc.operator.query.ops.status",
      execution: "query",
      request: "OpsStatusRequest",
      success: "OperationStatusSnapshot",
      error: "OpsStatusError",
      response: "OpsStatusResponse",
    },
    {
      name: "ops.watch",
      subject: "plz.v1.rpc.operator.query.ops.watch",
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
  assert.equal(
    certBundleRef(
      "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef:/var/lib/ployz/certs/cert_api.pem",
    ),
    "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef:/var/lib/ployz/certs/cert_api.pem",
  );
  assert.throws(() => certBundleRef("obj://PLZ_CERTS/cert_api/rev_1"));
  assert.equal(machineBootstrapUrl("https://get.ployz.sh"), "https://get.ployz.sh");
  assert.throws(() => machineBootstrapUrl("http://get.ployz.sh"));
  assert.equal(machineJoinToken("join_once_123"), "join_once_123");
  assert.throws(() => machineJoinToken("join token"));
});

class RecordingTransport implements PloyzOperationTransport {
  readonly requestEndpoints: PloyzApiEndpoint[] = [];
  readonly deployReserveRequests: DeployReserveRequest[] = [];
  readonly deployRequests: DeploySubmitRequest[] = [];
  readonly initFirstMachineActivateRequests: InitFirstMachineActivateRequest[] = [];
  readonly machineAddRequests: MachineAddRequest[] = [];
  readonly machineUpdateRequests: MachineUpdateRequest[] = [];
  readonly machineListRequests: MachineListRequest[] = [];
  readonly machineInspectRequests: MachineInspectRequest[] = [];
  readonly machineJoinRedeemRequests: MachineJoinRedeemRequest[] = [];
  readonly serviceListRequests: ServiceListRequest[] = [];
  readonly serviceInspectRequests: ServiceInspectRequest[] = [];
  readonly runtimeSnapshotRequests: RuntimeSnapshotRequest[] = [];
  readonly logsTailRequests: LogsTailRequest[] = [];
  readonly opsListRequests: OpsListRequest[] = [];
  readonly statusRequests: OpsStatusRequest[] = [];
  readonly watchRequests: OpsWatchRequest[] = [];
  readonly accepted: AcceptedOperation;
  readonly deployReserved: DeployReserved;
  readonly firstMachineActivated: InitFirstMachineActivated;
  readonly machineAddAccepted: MachineAddAccepted;
  readonly machineSnapshots: MachineSnapshot[];
  readonly machineJoinRedeemed: MachineJoinRedeemed;
  readonly serviceSnapshots: ServiceSnapshot[];
  readonly runtimeSnapshot: RuntimeSnapshot;
  readonly status: OperationStatusSnapshot;
  readonly replay: OperationEventReplayPage;
  readonly replayPages: OperationEventReplayPage[] = [];
  deployReserveResponse?: DeployReserveResponse;
  deployResponse?: DeploySubmitResponse;
  initFirstMachineActivateResponse?: InitFirstMachineActivateResponse;
  machineAddResponse?: MachineAddResponse;
  machineUpdateResponse?: MachineUpdateResponse;
  machineListResponse?: MachineListResponse;
  machineInspectResponse?: MachineInspectResponse;
  machineJoinRedeemResponse?: MachineJoinRedeemResponse;
  serviceListResponse?: ServiceListResponse;
  serviceInspectResponse?: ServiceInspectResponse;
  runtimeSnapshotResponse?: RuntimeSnapshotResponse;
  logsTailResponse?: LogsTailResponse;
  statusResponse?: OpsStatusResponse;
  watchResponse?: OpsWatchResponse;

  constructor(fixture: OperationFixture) {
    this.accepted = fixture.accepted_operation;
    this.deployReserved = fixture.deploy_reserve_response.value;
    this.firstMachineActivated = fixture.init_first_machine_activate_response.value;
    this.machineAddAccepted = fixture.machine_add_response.value;
    this.machineSnapshots = fixture.machine_snapshots;
    this.machineJoinRedeemed = fixture.machine_join_redeem_response.value;
    this.serviceSnapshots = fixture.service_snapshots;
    this.runtimeSnapshot = fixture.runtime_snapshot;
    this.status = fixture.operation_status_snapshot;
    this.replay = fixture.operation_event_replay_page;
  }

  async request(endpoint: PloyzApiEndpoint, request: unknown): Promise<any> {
    this.requestEndpoints.push(endpoint);
    switch (endpoint) {
      case "deploy.reserve":
        this.deployReserveRequests.push(request as DeployReserveRequest);
        return (this.deployReserveResponse ?? {
          status: "ok",
          value: this.deployReserved,
        }) as any;
      case "deploy.submit":
        this.deployRequests.push(request as DeploySubmitRequest);
        return (this.deployResponse ?? { status: "ok", value: this.accepted }) as any;
      case "init.first_machine.activate":
        this.initFirstMachineActivateRequests.push(request as InitFirstMachineActivateRequest);
        return (this.initFirstMachineActivateResponse ?? {
          status: "ok",
          value: this.firstMachineActivated,
        }) as any;
      case "machine.add":
        this.machineAddRequests.push(request as MachineAddRequest);
        return (this.machineAddResponse ?? { status: "ok", value: this.machineAddAccepted }) as any;
      case "machine.drain":
      case "machine.resume":
        return { status: "ok", value: this.accepted } as any;
      case "core.replace":
      case "core.replace.report":
        throw new Error("core replace is not used by ergonomic client tests");
      case "machine.update":
        this.machineUpdateRequests.push(request as MachineUpdateRequest);
        return (this.machineUpdateResponse ?? { status: "ok", value: this.accepted }) as any;
      case "machine.list":
        this.machineListRequests.push(request as MachineListRequest);
        return (this.machineListResponse ?? {
          status: "ok",
          value: { machines: this.machineSnapshots },
        }) as any;
      case "machine.inspect":
        this.machineInspectRequests.push(request as MachineInspectRequest);
        return (this.machineInspectResponse ?? {
          status: "ok",
          value: this.machineSnapshots[0],
        }) as any;
      case "machine.redeem":
        this.machineJoinRedeemRequests.push(request as MachineJoinRedeemRequest);
        return (this.machineJoinRedeemResponse ?? {
          status: "ok",
          value: this.machineJoinRedeemed,
        }) as any;
      case "machine.report":
        throw new Error("machine join report is not used by ergonomic client tests");
      case "service.list":
        this.serviceListRequests.push(request as ServiceListRequest);
        return (this.serviceListResponse ?? {
          status: "ok",
          value: { services: this.serviceSnapshots },
        }) as any;
      case "service.inspect":
        this.serviceInspectRequests.push(request as ServiceInspectRequest);
        return (this.serviceInspectResponse ?? {
          status: "ok",
          value: this.serviceSnapshots[0],
        }) as any;
      case "runtime.snapshot":
        this.runtimeSnapshotRequests.push(request as RuntimeSnapshotRequest);
        return (this.runtimeSnapshotResponse ?? {
          status: "ok",
          value: { snapshot: this.runtimeSnapshot },
        }) as any;
      case "logs.tail": {
        const logsRequest = request as LogsTailRequest;
        this.logsTailRequests.push(logsRequest);
        return (this.logsTailResponse ?? {
          status: "ok",
          value: {
            target:
              logsRequest.target.target === "container"
                ? {
                    target: "container" as const,
                    machine_id: machineId("machine_a"),
                    container_id: logsRequest.target.container_id,
                  }
                : logsRequest.target,
            text: "hello\n",
            truncated: false,
          },
        }) as any;
      }
      case "ops.list":
        this.opsListRequests.push(request as OpsListRequest);
        return ({
          status: "ok",
          value: { operations: [] },
        } as any);
      case "ops.status":
        this.statusRequests.push(request as OpsStatusRequest);
        return (this.statusResponse ?? { status: "ok", value: this.status }) as any;
      case "ops.watch":
        this.watchRequests.push(request as OpsWatchRequest);
        return (this.watchResponse ?? {
          status: "ok",
          value: this.replayPages.shift() ?? this.replay,
        }) as any;
    }
  }
}

interface OperationFixture {
  accepted_operation: AcceptedOperation;
  deploy_reserve_response: { status: "ok"; value: DeployReserved };
  init_first_machine_activate_response: { status: "ok"; value: InitFirstMachineActivated };
  machine_add_response: { status: "ok"; value: MachineAddAccepted };
  machine_snapshots: MachineSnapshot[];
  machine_join_redeem_response: { status: "ok"; value: MachineJoinRedeemed };
  service_snapshots: ServiceSnapshot[];
  runtime_snapshot: RuntimeSnapshot;
  operation_status_snapshot: OperationStatusSnapshot;
  operation_event_replay_page: OperationEventReplayPage;
}

function defaultFixture(): OperationFixture {
  return {
    accepted_operation: acceptedOperation("op_123"),
    deploy_reserve_response: {
      status: "ok",
      value: {
        reservation_id: deployReservationId(1),
        expires_at: "4102444800" as DeployReserved["expires_at"],
      },
    },
    init_first_machine_activate_response: {
      status: "ok",
      value: {
        operation_id: operationId("op_init_core_1"),
        machine_id: machineId("core_1"),
      },
    },
    machine_add_response: {
      status: "ok",
      value: {
        accepted: {
          ...acceptedOperation("op_machine"),
          start_sequence: eventSequence(7),
        },
        machine_id: machineId("machine_2"),
        bootstrap_url: machineBootstrapUrl("https://get.ployz.sh"),
        join_bundle: machineJoinBundle(),
        join_token: machineJoinToken("join_once_123"),
        join_secret_delivery: machineJoinSecretDelivery(),
      },
    },
    machine_snapshots: [
      {
        active: {
          lifecycle: "active" as const,
          machine_id: machineId("machine_2"),
          name: machineName("edge_2"),
          activated_by: operationId("op_machine"),
          control_endpoints: [],
          mesh_endpoints: [],
          endpoint_subnet: "10.42.2.0/24",
        },
        testimony: { status: "no_answer" as const },
      },
    ],
    machine_join_redeem_response: {
      status: "ok",
      value: {
        operation_id: operationId("op_machine"),
        machine_id: machineId("machine_2"),
        name: machineName("edge_2"),
        roles: gatewaySkippedRoles(),
        join_bundle: machineJoinBundle(),
        secret_delivery: machineJoinSecretDelivery(),
        joined_at: "60" as MachineJoinRedeemed["joined_at"],
        last_event_sequence: eventSequence(8),
        result: "joined",
      },
    },
    service_snapshots: [
      {
        active: {
          namespace_id: namespaceId("default"),
          service_id: serviceId("svc_api"),
          namespace_revision_entry_id: namespaceRevisionEntryId("rev_2"),
          image: imageReference("nginx:1.27-alpine"),
          desired_replicas: replicaCount(1),
          volume_names: [],
        },
        route_bindings: [],
        testimony: {
          ready_container_count: 0,
          observed_container_count: 0,
          machines: [],
        },
      },
    ],
    runtime_snapshot: {
      machines: [
        {
          active: {
            lifecycle: "active" as const,
            machine_id: machineId("machine_2"),
            name: machineName("edge_2"),
            activated_by: operationId("op_machine"),
            control_endpoints: [],
            mesh_endpoints: [],
            endpoint_subnet: "10.42.2.0/24",
          },
          testimony: { status: "no_answer" as const },
        },
      ],
      services: [
        {
          active: {
            namespace_id: namespaceId("default"),
            service_id: serviceId("svc_api"),
            namespace_revision_entry_id: namespaceRevisionEntryId("rev_2"),
            image: imageReference("nginx:1.27-alpine"),
            desired_replicas: replicaCount(1),
            volume_names: [],
          },
          route_bindings: [],
          testimony: {
            ready_container_count: 0,
            observed_container_count: 0,
            machines: [],
          },
        },
      ],
      routes: [],
      containers: [],
      revisions: [
        {
          namespace_id: namespaceId("default"),
          service_id: serviceId("svc_api"),
          namespace_revision_entry_id: namespaceRevisionEntryId("rev_2"),
        },
      ],
      releases: [
        {
          namespace_id: namespaceId("default"),
          service_id: serviceId("svc_api"),
          namespace_revision_entry_id: namespaceRevisionEntryId("rev_2"),
          routes: [],
        },
      ],
      instances: [],
      projection_sources: {
        intent: { read_at_unix_seconds: 1 },
        facts: { read_at_unix_seconds: 1 },
        revisions: { status: "complete", source_count: 1, missing_link_count: 0 },
        releases: { status: "complete", source_count: 1, missing_link_count: 0 },
        instances: { status: "complete", source_count: 0, missing_link_count: 0 },
      },
      updated_at_unix_seconds: 1,
    },
    operation_status_snapshot: {
      status: {
        kind: "deploy",
        id: operationId("op_123"),
        namespace_id: namespaceId("default"),
        service_id: serviceId("svc_api"),
        state: { state: "accepted" },
        last_event_sequence: eventSequence(11),
      }
    },
    operation_event_replay_page: {
      events: [],
      cursor: { state: "caught_up" },
    },
  };
}

function deployInput() {
  return {
    idempotencyKey: "idem_deploy_123",
    serviceId: "svc_api",
    image: "ghcr.io/acme/api:rev-2",
    replicas: 1,
  };
}

function machineAddInput() {
  return {
    operationId: "op_machine",
    idempotencyKey: "idem_machine",
    machineId: "machine_2",
    name: "edge_2",
    roles: gatewaySkippedRoles(),
  };
}

function machineUpdateInput() {
  return {
    operationId: "op_update_machine_2_123",
    machineId: "machine_2",
    targetVersion: "0.2.0",
  };
}

function installAllRoles() {
  return { gateway: "install" as const };
}

function gatewaySkippedRoles() {
  return { gateway: "skip" as const };
}

function machineJoinBundle(): MachineJoinBundle {
  return {
    material: {
      cluster_name: "prod",
      dataplane_endpoint_supernet: "10.198.0.0/16",
      runtime_nats_url: "nats://127.0.0.1:7422",
      trusted_nats: {
        ca_pem:
          "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
      },
      recovery_key_wrapped: [1, 2, 3],
      core_seeds_wrapped: [4, 5, 6],
      ployzd: {
        version: "0.1.0",
        source: "/tmp/ployzd",
        sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        install_path: "/usr/local/bin/ployzd",
      },
      ebpf_bytecode: machineJoinArtifact(
        "/tmp/ployz-ebpf-tc",
        "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
      ),
      ebpf_ctl: machineJoinArtifact("/tmp/ployz-ebpf-ctl", "/usr/local/bin/ployz-ebpf-ctl"),
    },
  };
}

function machineJoinArtifact(source: string, installPath: string) {
  return {
    version: "0.1.0",
    source,
    sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    install_path: installPath,
  };
}

function machineJoinSecretDelivery(): MachineJoinSecretDelivery {
  return {
    nats_credentials: "SUAIZ5LKGG2Y4WC7ZPKS46LSLLJQIFTO6KMSWSU2VN3TC7YRRIKH5WRXJQ",
  };
}

function acceptedOperation(operationIdValue: string): AcceptedOperation {
  return {
    operation_id: operationId(operationIdValue),
    watch_subject: `plz.v1.progress.namespace.default.operation.${operationIdValue}.>`,
    start_sequence: eventSequence(11),
  };
}
