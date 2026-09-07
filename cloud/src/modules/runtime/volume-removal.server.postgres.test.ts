import type { MachineId } from "@ployz/sdk";
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
  cancelVolumeRemoveAttemptActivity,
  completeVolumeRemoveAttemptActivity,
  dispatchVolumeRemoveRequested,
  loadLatestVolumeRemoveAttempt,
  prepareVolumeRemoveAttemptActivity,
  reconcileVolumeRemoveTombstoneActivity,
} from "#/modules/runtime/volume-removal.server";
import {
  insertVolumeRemoveAttempt,
  type VolumeRemoveAttempt,
} from "#/modules/runtime/volume-removal.repository";

const organizationId = "00000000-0000-4000-8000-000000000601";
const userId = "00000000-0000-4000-8000-000000000602";
const otherUserId = "00000000-0000-4000-8000-000000000603";
const projectId = "00000000-0000-4000-8000-000000000604";
const environmentId = "00000000-0000-4000-8000-000000000605";
const lineageId = "00000000-0000-4000-8000-000000000606";
const resourceId = "00000000-0000-4000-8000-000000000607";
const volume = {
  machine_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" as MachineId,
  name: `vol-${resourceId}`,
};

async function insertAttempt(
  harness: GithubPostgresTestHarness,
  retryOfAttemptId?: string,
) {
  return harness.runEffect(
    insertVolumeRemoveAttempt({
      organizationId,
      requestedByUserId: userId,
      environmentId,
      environmentResourceId: resourceId,
      volumes: [volume],
      retryOfAttemptId,
    }),
  );
}

describe("direct volume removal durable state", () => {
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
      values ('${organizationId}', 'Volumes', 'volumes');
      insert into "user" (id, email, name) values
        ('${userId}', 'volumes@example.com', 'Owner'),
        ('${otherUserId}', 'other@example.com', 'Other');
      insert into member (id, organization_id, user_id, role, created_at)
      values (gen_random_uuid(), '${organizationId}', '${userId}', 'owner', now());
      insert into project (id, organization_id, name, slug)
      values ('${projectId}', '${organizationId}', 'Runtime', 'runtime');
      insert into environment (id, project_id, organization_id, name, namespace)
      values ('${environmentId}', '${projectId}', '${organizationId}', 'Production', 'runtime-production');
      insert into resource_lineage (
        id, organization_id, project_id, canonical_name, canonical_slug
      ) values ('${lineageId}', '${organizationId}', '${projectId}', 'Data', 'data');
      insert into environment_resource (
        id, organization_id, project_id, environment_id, lineage_id,
        implementation_type, name, slug, deleted_at
      ) values (
        '${resourceId}', '${organizationId}', '${projectId}', '${environmentId}',
        '${lineageId}', 'volume', 'Data', 'data', now()
      );
    `);
  });

  it("preserves a pending row when post-commit dispatch fails", async () => {
    const attempt = await insertAttempt(harness);
    const failing = new Inngest({ id: "volume-remove-dispatch-fail-postgres" });
    failing.send = async () => {
      throw new Error("Inngest unavailable");
    };
    const exit = await runPromiseDb(
      dispatchVolumeRemoveRequested(attempt.id).pipe(
        Effect.provideService(InngestClient, failing),
        Effect.exit,
      ),
    );
    expect(Exit.isFailure(exit)).toBe(true);

    const rows = await harness.pool.query<{
      status: string;
      inngest_run_id: string | null;
    }>(
      "select status, inngest_run_id from volume_remove_attempt where id = $1",
      [attempt.id],
    );
    expect(rows.rows).toEqual([{ status: "pending", inngest_run_id: null }]);
  });

  it("authorizes reads through Actor membership and the managed database", async () => {
    const attempt = await insertAttempt(harness);
    const authorized = await harness.runEffect(
      loadLatestVolumeRemoveAttempt(
          { userId },
          {
            organizationSlug: "volumes",
            environmentId,
            resourceId,
          },
        ),
    );
    expect(authorized?.id).toBe(attempt.id);

    const denied = await harness.runEffect(
      loadLatestVolumeRemoveAttempt(
          { userId: otherUserId },
          {
            organizationSlug: "volumes",
            environmentId,
            resourceId,
          },
        ).pipe(Effect.exit),
    );
    expect(Exit.isFailure(denied)).toBe(true);
  });

  it("binds provider completion, replay, and tombstone reconciliation to one run", async () => {
    const attempt = await insertAttempt(harness);
    const prepared = await runPromiseDb(
      prepareVolumeRemoveAttemptActivity({
        attemptId: attempt.id,
        inngestRunId: "run-1",
        now: new Date("2026-09-04T03:00:00Z"),
      }),
    );
    expect(prepared.kind).toBe("ready");
    const replayed = await runPromiseDb(
      prepareVolumeRemoveAttemptActivity({
        attemptId: attempt.id,
        inngestRunId: "run-1",
        now: new Date("2026-09-04T03:01:00Z"),
      }),
    );
    expect(replayed.kind).toBe("ready");
    const stolen = await runPromiseDb(
      prepareVolumeRemoveAttemptActivity({
        attemptId: attempt.id,
        inngestRunId: "run-other",
        now: new Date("2026-09-04T03:02:00Z"),
      }).pipe(Effect.exit),
    );
    expect(Exit.isFailure(stolen)).toBe(true);

    const completed = await runPromiseDb(
      completeVolumeRemoveAttemptActivity({
        attemptId: attempt.id,
        inngestRunId: "run-1",
        status: "completed",
        outcome: { destroyed: [volume], failed: [], omitted: [] },
        now: new Date("2026-09-04T03:03:00Z"),
      }),
    );
    await harness.runEffect(reconcileVolumeRemoveTombstoneActivity(completed));

    const resource = await harness.pool.query(
      "select id from environment_resource where id = $1",
      [resourceId],
    );
    expect(resource.rowCount).toBe(0);
  });

  it("terminalizes cancellation by exact Inngest run identity", async () => {
    const attempt = await insertAttempt(harness);
    await runPromiseDb(
      prepareVolumeRemoveAttemptActivity({
        attemptId: attempt.id,
        inngestRunId: "run-cancel",
        now: new Date("2026-09-04T04:00:00Z"),
      }),
    );
    await runPromiseDb(
      cancelVolumeRemoveAttemptActivity({
        inngestRunId: "run-cancel",
        now: new Date("2026-09-04T04:01:00Z"),
      }),
    );

    const rows = await harness.pool.query<Pick<VolumeRemoveAttempt, "status" | "inngestRunId">>(
      `select status, inngest_run_id as "inngestRunId"
       from volume_remove_attempt where id = $1`,
      [attempt.id],
    );
    expect(rows.rows).toEqual([
      { status: "cancelled", inngestRunId: "run-cancel" },
    ]);
  });
});
