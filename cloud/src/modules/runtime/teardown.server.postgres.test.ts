import { Effect, Exit } from "effect";
import { Inngest } from "inngest";
import {
  afterAll,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
} from "vitest";
import {
  type GithubPostgresTestHarness,
  startGithubPostgresTestHarness,
} from "#/modules/github/github-ingestion.postgres-test-harness";
import { InngestClient } from "#/modules/inngest/client";
import {
  cancelTeardownAttemptActivity,
  completeTeardownAttemptActivity,
  dropTeardownCloudRowsActivity,
  failOwnedTeardownAttemptActivity,
  prepareTeardownAttemptActivity,
} from "#/modules/runtime/teardown-activities.server";
import { insertTeardownAttempt } from "#/modules/runtime/teardown.repository";
import { dispatchTeardownRequested } from "#/modules/runtime/teardown.server";

const organizationId = "00000000-0000-4000-8000-000000000701";
const userId = "00000000-0000-4000-8000-000000000702";
const projectId = "00000000-0000-4000-8000-000000000703";
const environmentId = "00000000-0000-4000-8000-000000000704";

async function insertAttempt(harness: GithubPostgresTestHarness) {
  return harness.runEffect(
    insertTeardownAttempt({
      organizationId,
      requestedByUserId: userId,
      projectId,
      environmentId,
      scope: "environment",
      confirmDataLoss: [],
      targets: {
        environments: [
          {
            environmentId,
            projectId,
            namespace: "app-production",
            cloudName: "acme/app/Production",
            identities: [],
          },
        ],
        machines: [],
        revokePairing: false,
        runtimeMembership: "untouched",
      },
    }),
  );
}

describe("teardown durable state", () => {
  let harness: GithubPostgresTestHarness;

  function runPromiseDb<A, E>(
    operation: Effect.Effect<A, E, import("#/server/database.server").Database>,
  ) {
    return harness.runEffect(operation);
  }

  beforeAll(async () => {
    harness = await startGithubPostgresTestHarness();
  }, 60_000);

  afterAll(async () => {
    await harness.stop();
  });

  beforeEach(async () => {
    await harness.pool.query(`
      truncate table organization, "user" cascade;
      insert into organization (id, name, slug)
      values ('${organizationId}', 'Acme', 'acme');
      insert into "user" (id, email, name)
      values ('${userId}', 'teardown@example.com', 'Owner');
      insert into member (id, organization_id, user_id, role, created_at)
      values (gen_random_uuid(), '${organizationId}', '${userId}', 'owner', now());
      insert into project (id, organization_id, name, slug)
      values ('${projectId}', '${organizationId}', 'App', 'app');
      insert into environment (id, project_id, organization_id, name, namespace, intent
      ) values ('${environmentId}', '${projectId}', '${organizationId}', 'Production', 'app-production', '{"version":1,"environmentSlug":"app-production","services":[],"variableGroups":[],"volumes":[]}'
      );
    `);
  });

  it("preserves a pending row when post-commit dispatch fails", async () => {
    const attempt = await insertAttempt(harness);
    const failing = new Inngest({ id: "teardown-dispatch-fail-postgres" });
    failing.send = async () => {
      throw new Error("Inngest unavailable");
    };
    const exit = await runPromiseDb(
      dispatchTeardownRequested(attempt.id).pipe(
        Effect.provideService(InngestClient, failing),
        Effect.exit,
      ),
    );
    expect(Exit.isFailure(exit)).toBe(true);

    const rows = await harness.pool.query<{
      status: string;
      inngest_run_id: string | null;
    }>(
      "select status, inngest_run_id from teardown_attempt where id = $1",
      [attempt.id],
    );
    expect(rows.rows).toEqual([{ status: "pending", inngest_run_id: null }]);
  });

  it("claims once, rejects another owner, drops Cloud rows, then completes", async () => {
    const attempt = await insertAttempt(harness);
    const claimed = await runPromiseDb(
      prepareTeardownAttemptActivity({
        attemptId: attempt.id,
        inngestRunId: "run-1",
        now: new Date("2026-09-04T05:00:00Z"),
      }),
    );
    expect(claimed.kind).toBe("ready");
    const replay = await runPromiseDb(
      prepareTeardownAttemptActivity({
        attemptId: attempt.id,
        inngestRunId: "run-1",
        now: new Date("2026-09-04T05:01:00Z"),
      }),
    );
    expect(replay.kind).toBe("ready");
    const stolen = await runPromiseDb(
      prepareTeardownAttemptActivity({
        attemptId: attempt.id,
        inngestRunId: "run-other",
        now: new Date("2026-09-04T05:02:00Z"),
      }).pipe(Effect.exit),
    );
    expect(Exit.isFailure(stolen)).toBe(true);

    if (claimed.kind !== "ready") return;
    await harness.runEffect(dropTeardownCloudRowsActivity(claimed.attempt));
    await runPromiseDb(
      completeTeardownAttemptActivity({
        attemptId: attempt.id,
        inngestRunId: "run-1",
        status: "completed",
        outcome: {
          rustMustRevokePairing: false,
          runtimeMembership: "untouched",
        },
        now: new Date("2026-09-04T05:03:00Z"),
      }),
    );

    const state = await harness.pool.query<{ status: string }>(
      "select status from teardown_attempt where id = $1",
      [attempt.id],
    );
    const environment = await harness.pool.query(
      "select id from environment where id = $1",
      [environmentId],
    );
    expect(state.rows).toEqual([{ status: "completed" }]);
    expect(environment.rowCount).toBe(0);
  });

  it("terminalizes failure and cancellation only for their owning runs", async () => {
    const failedAttempt = await insertAttempt(harness);
    await runPromiseDb(
      prepareTeardownAttemptActivity({
        attemptId: failedAttempt.id,
        inngestRunId: "run-fail",
        now: new Date("2026-09-04T06:00:00Z"),
      }),
    );
    const wrongOwner = await runPromiseDb(
      failOwnedTeardownAttemptActivity({
        attemptId: failedAttempt.id,
        inngestRunId: "run-other",
        failureMessage: "wrong owner",
        now: new Date("2026-09-04T06:01:00Z"),
      }),
    );
    expect(wrongOwner).toEqual({ state: "skipped" });
    await runPromiseDb(
      failOwnedTeardownAttemptActivity({
        attemptId: failedAttempt.id,
        inngestRunId: "run-fail",
        failureMessage: "retries exhausted",
        now: new Date("2026-09-04T06:02:00Z"),
      }),
    );

    const cancelledAttempt = await insertAttempt(harness);
    await runPromiseDb(
      prepareTeardownAttemptActivity({
        attemptId: cancelledAttempt.id,
        inngestRunId: "run-cancel",
        now: new Date("2026-09-04T06:03:00Z"),
      }),
    );
    await runPromiseDb(
      cancelTeardownAttemptActivity({
        inngestRunId: "run-cancel",
        now: new Date("2026-09-04T06:04:00Z"),
      }),
    );

    const states = await harness.pool.query<{
      status: string;
      inngest_run_id: string;
    }>(
      `select status, inngest_run_id from teardown_attempt order by created_at`,
    );
    expect(states.rows).toEqual([
      { status: "failed", inngest_run_id: "run-fail" },
      { status: "cancelled", inngest_run_id: "run-cancel" },
    ]);
  });
});
