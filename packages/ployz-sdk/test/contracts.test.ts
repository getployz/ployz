import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  MAX_OPERATION_EVENT_REPLAY_LIMIT,
  OPERATION_API_CONTRACTS,
  eventSequence,
  operationEventReplayLimit,
  operationId,
} from "../src/index.ts";

const fixturePath = join(
  dirname(fileURLToPath(import.meta.url)),
  "fixtures",
  "operation-contract.json",
);

test("generated registry describes transport-neutral operation endpoints", () => {
  assert.ok(OPERATION_API_CONTRACTS.some(({ name }) => name === "deploy.submit"));
  for (const contract of OPERATION_API_CONTRACTS) {
    assert.equal("subject" in contract, false);
    assert.equal("execution" in contract, false);
  }
});

test("operation primitives enforce the Rust wire constraints", () => {
  assert.equal(operationId("op_123"), "op_123");
  assert.equal(eventSequence(42n), "42");
  assert.equal(
    operationEventReplayLimit(MAX_OPERATION_EVENT_REPLAY_LIMIT),
    MAX_OPERATION_EVENT_REPLAY_LIMIT,
  );

  assert.throws(() => operationId("bad.id"), RangeError);
  assert.throws(() => eventSequence(0), RangeError);
  assert.throws(
    () => operationEventReplayLimit(MAX_OPERATION_EVENT_REPLAY_LIMIT + 1),
    RangeError,
  );
});

test("representative fixture remains transport neutral", () => {
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as Record<string, unknown>;

  assert.deepEqual(fixture.accepted_operation, {
    operation_id: "op_123",
    start_sequence: "1",
  });
  assert.equal("operation_subject" in fixture, false);
});
