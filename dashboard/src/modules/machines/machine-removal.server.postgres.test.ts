import { Effect, Result } from "effect";
import {
  afterAll,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
} from "vitest";
import { asTestDouble } from "#/lib/test-double";
import {
  type PostgresTestHarness,
  startPostgresTestHarness,
} from "#/test/postgres";
import { InngestClient } from "#/modules/inngest/client";
import {
  cancelMachineRemoveAttemptActivity,
  completeMachineRemoveAttemptActivity,
  failOwnedMachineRemoveAttemptActivity,
  prepareMachineRemoveAttemptActivity,
  startMachineRemove,
} from "#/modules/machines/machine-removal.server";
import { requestMachineRemoveAttempt } from "#/modules/machines/machine-removal.repository";
import { Conflict } from "#/server/public-error";
import type { Inngest } from "inngest";

const organizationId = "00000000-0000-4000-8000-000000000501";
const userId = "00000000-0000-4000-8000-000000000502";

async function request(harness: PostgresTestHarness, machineId: string) {
  return harness.runEffect(
    requestMachineRemoveAttempt({
      organizationId,
      requestedByUserId: userId,
      machineId,
      confirmDataLoss: [],
    }),
  );
}

describe("machine removal durable ownership", () => {
  let harness: PostgresTestHarness;

  function runEffect<A, E>(operation: Effect.Effect<A, E, import("#/server/database.server").Database>) {
    return harness.runEffect(operation);
  }

  beforeAll(async () => {
    harness = await startPostgresTestHarness();
  }, 60_000);

  afterAll(async () => {
    await harness.stop();
  });

  beforeEach(async () => {
    await harness.pool.query(`
      truncate table machine_remove_attempt, operation_outbox, "user", organization cascade;
      insert into organization (id, name, slug)
      values ('${organizationId}', 'Remove', 'remove');
      insert into "user" (id, email, name)
      values ('${userId}', 'remove@example.com', 'Owner');
    `);
  });

  it("keeps the attempt and its unsent event when delivery after commit fails", async () => {
    const failingInngest = asTestDouble<Inngest>()({
      send: async () => {
        throw new Error("Inngest unavailable");
      },
    });

    const attempt = await runEffect(
      startMachineRemove({
        organizationId,
        requestedByUserId: userId,
        machineId: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        confirmDataLoss: [],
      }).pipe(Effect.provideService(InngestClient, failingInngest)),
    );

    const rows = await harness.pool.query("select state from machine_remove_attempt where id = $1", [attempt.id]);
    expect(rows.rows).toEqual([{ state: "pending" }]);
    const outbox = await harness.pool.query("select event_id, sent_at from operation_outbox");
    expect(outbox.rows).toEqual([{ event_id: `machine-remove-${attempt.id}`, sent_at: null }]);
  });

  it("rejects a second active remove for the same organization machine as Conflict", async () => {
    await request(harness, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const duplicate = await runEffect(
      Effect.result(
        requestMachineRemoveAttempt({
          organizationId,
          requestedByUserId: userId,
          machineId: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          confirmDataLoss: [],
        }),
      ),
    );

    expect(Result.isFailure(duplicate)).toBe(true);
    if (!Result.isFailure(duplicate)) return;
    expect(duplicate.failure).toBeInstanceOf(Conflict);
    expect(duplicate.failure._tag).toBe("Conflict");
  });

  it("binds replay, completion, failure, and cancellation to the exact run", async () => {
    const attempt = await request(harness, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const prepared = await runEffect(
      prepareMachineRemoveAttemptActivity({
        attemptId: attempt.id,
        inngestRunId: "run-1",
        now: new Date("2026-09-04T01:00:00Z"),
      }),
    );
    expect(prepared.kind).toBe("ready");

    const replayed = await runEffect(
      prepareMachineRemoveAttemptActivity({
        attemptId: attempt.id,
        inngestRunId: "run-1",
        now: new Date("2026-09-04T01:01:00Z"),
      }),
    );
    expect(replayed.kind).toBe("ready");

    const stolen = await runEffect(
      Effect.result(
        prepareMachineRemoveAttemptActivity({
          attemptId: attempt.id,
          inngestRunId: "run-2",
          now: new Date("2026-09-04T01:02:00Z"),
        }),
      ),
    );
    expect(Result.isFailure(stolen)).toBe(true);
    if (!Result.isFailure(stolen)) return;
    expect(stolen.failure).toBeInstanceOf(Conflict);

    await runEffect(
      completeMachineRemoveAttemptActivity({
        attemptId: attempt.id,
        inngestRunId: "run-1",
        completion: { state: "succeeded", result: { resetWarning: null, release: { kind: "others_remain" } } },
        now: new Date("2026-09-04T01:03:00Z"),
      }),
    );
    await runEffect(
      completeMachineRemoveAttemptActivity({
        attemptId: attempt.id,
        inngestRunId: "run-1",
        completion: { state: "succeeded", result: { resetWarning: null, release: { kind: "others_remain" } } },
        now: new Date("2026-09-04T01:04:00Z"),
      }),
    );

    const cancellable = await request(harness, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    await runEffect(
      prepareMachineRemoveAttemptActivity({
        attemptId: cancellable.id,
        inngestRunId: "run-cancel",
        now: new Date("2026-09-04T02:00:00Z"),
      }),
    );
    const wrongOwner = await runEffect(
      failOwnedMachineRemoveAttemptActivity({
        attemptId: cancellable.id,
        inngestRunId: "run-other",
        now: new Date("2026-09-04T02:01:00Z"),
      }),
    );
    expect(wrongOwner).toEqual({ state: "skipped" });
    await runEffect(
      cancelMachineRemoveAttemptActivity({
        inngestRunId: "run-cancel",
        now: new Date("2026-09-04T02:02:00Z"),
      }),
    );

    const states = await harness.pool.query<{
      id: string;
      state: string;
      inngest_run_id: string;
    }>(
      `select id, state, inngest_run_id
       from machine_remove_attempt order by machine_id`,
    );
    expect(states.rows).toEqual([
      { id: attempt.id, state: "succeeded", inngest_run_id: "run-1" },
      {
        id: cancellable.id,
        state: "cancelled",
        inngest_run_id: "run-cancel",
      },
    ]);
  });
});
