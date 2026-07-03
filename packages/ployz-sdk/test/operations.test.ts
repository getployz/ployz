import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  eventSequence,
  machineId,
  machineName,
  MAX_OPERATION_EVENT_REPLAY_LIMIT,
  namespaceId,
  OPERATION_API_CONTRACTS,
  operationEventReplayLimit,
  operationId,
  revisionId,
  serviceId,
} from "../src/index.ts";
import type {
  AcceptedOperation,
  MachineJoinBundle,
  MachineJoinRedeemed,
  MachineJoinSecretDelivery,
  MachineSnapshot,
  OperationEventReplayPage,
  OperationStatusSnapshot,
  OperationSubject,
  RuntimeSnapshot,
  ServiceSnapshot,
} from "../src/index.ts";

const fixturePath = join(
  dirname(fileURLToPath(import.meta.url)),
  "fixtures",
  "operation-contract.json",
);

test("TypeScript DTOs match the Rust-emitted operation fixture", () => {
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as OperationFixture;

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

test("sdk exports operation subjects", () => {
  const subject: OperationSubject = {
    kind: "deploy",
    service_id: serviceId("svc_api"),
  };

  assert.deepEqual(subject, { kind: "deploy", service_id: "svc_api" });
});

test("sdk exports only operation API contracts", () => {
  assert.equal(
    OPERATION_API_CONTRACTS.some((contract) => contract.name.startsWith("machine.container.")),
    false,
  );
  assert.deepEqual(
    OPERATION_API_CONTRACTS.map((contract) => contract.name),
    [
      "deploy.submit",
      "init.first_machine.activate",
      "machine.add",
      "machine.list",
      "machine.inspect",
      "machine.join.redeem",
      "machine.join.report",
      "service.list",
      "service.inspect",
      "runtime.snapshot",
      "logs.tail",
      "ops.status",
      "ops.watch",
      "backup.create",
    ],
  );
});

test("sdk helpers brand primitive wire values", () => {
  assert.equal(operationEventReplayLimit(MAX_OPERATION_EVENT_REPLAY_LIMIT), 512);
  assert.equal(eventSequence("18446744073709551615"), "18446744073709551615");
  assert.equal(eventSequence(12n), "12");
  assert.equal(serviceId("svc_api"), "svc_api");
});

interface OperationFixture {
  accepted_operation: AcceptedOperation;
  machine_add_response: { status: "ok"; value: { join_bundle: MachineJoinBundle } };
  machine_snapshots: MachineSnapshot[];
  machine_join_redeem_response: { status: "ok"; value: MachineJoinRedeemed };
  service_snapshots: ServiceSnapshot[];
  runtime_snapshot: RuntimeSnapshot;
  operation_status_snapshot: OperationStatusSnapshot;
  operation_event_replay_page: OperationEventReplayPage;
  deploy_submit_response: { status: "ok"; value: AcceptedOperation };
  ops_status_response: { status: "ok"; value: OperationStatusSnapshot };
  ops_watch_response: { status: "ok"; value: OperationEventReplayPage };
}

const _typedFixtureExamples: {
  machine: MachineSnapshot;
  service: ServiceSnapshot;
  runtime: RuntimeSnapshot;
  joinSecret: MachineJoinSecretDelivery;
} = {
  machine: {
    active: {
      machine_id: machineId("machine_2"),
      name: machineName("edge_2"),
      activated_by: operationId("op_machine"),
    },
    public_ip: null,
    gateway: null,
    observed_container_count: 0,
  },
  service: {
    active: {
      namespace_id: namespaceId("default"),
      service_id: serviceId("svc_api"),
      active_revision: revisionId("rev_2"),
    },
  },
  runtime: {
    machines: [],
    services: [],
    routes: [],
    containers: [],
    revisions: [],
    releases: [],
    instances: [],
    projection_sources: {
      core_state: { read_at_unix_seconds: 1 },
      observations: { read_at_unix_seconds: 1 },
      revisions: { status: "complete", source_count: 0, missing_link_count: 0 },
      releases: { status: "complete", source_count: 0, missing_link_count: 0 },
      instances: { status: "complete", source_count: 0, missing_link_count: 0 },
    },
    updated_at_unix_seconds: 1,
  },
  joinSecret: {
    nats_credentials: "SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM",
  },
};
