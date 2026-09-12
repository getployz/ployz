import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { Inngest } from "inngest";
import * as schema from "#/db/schema";
import {
  type GithubPostgresTestHarness,
  startGithubPostgresTestHarness,
} from "#/modules/github/github-ingestion.postgres-test-harness";
import { Database } from "#/server/database.server";
import { InngestClient } from "#/modules/inngest/client";
import {
  beginEnvironmentDeploymentPlanning,
  ownsDeploymentRun,
  recordInngestRun,
} from "./runtime-lifecycle.repository.server";
import { markCancelledByInngestRunId, markDeploymentCancelled } from "./runtime-cancellation.repository.server";

const organizationId = "00000000-0000-4000-8000-000000000601";
const userId = "00000000-0000-4000-8000-000000000602";
const projectId = "00000000-0000-4000-8000-000000000603";
const environmentId = "00000000-0000-4000-8000-000000000604";
const savedId = "00000000-0000-4000-8000-000000000605";
const firstDeploymentId = "00000000-0000-4000-8000-000000000606";
const secondDeploymentId = "00000000-0000-4000-8000-000000000607";

const emptySavedIntent = {
  version: 1 as const,
  environmentSlug: "production",
  services: [],
  variableGroups: [],
  volumes: [],
};

describe("durable deployment activities", () => {
  let harness: GithubPostgresTestHarness;
  const inngest = new Inngest({ id: "durable-activities-test" });
  vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

  function runEffect<A, E>(
    operation: Effect.Effect<A, E, Database | InngestClient>,
  ) {
    return harness.runEffect(
      operation.pipe(Effect.provideService(InngestClient, inngest)),
    );
  }

  beforeAll(async () => {
    harness = await startGithubPostgresTestHarness();
  }, 60_000);

  afterAll(async () => {
    await harness?.stop();
  });

  beforeEach(async () => {
    await harness.pool.query(`
      truncate table environment_saved_state_snapshot, environment, project,
        "user", organization cascade;
      insert into organization (id, name, slug)
      values ('${organizationId}', 'Durable', 'durable');
      insert into "user" (id, email, name)
      values ('${userId}', 'owner@example.com', 'Owner');
      insert into project (id, organization_id, name, slug)
      values ('${projectId}', '${organizationId}', 'Cloud', 'cloud');
      insert into environment (
        id, project_id, organization_id, name, namespace, intent
      ) values (
        '${environmentId}', '${projectId}', '${organizationId}',
        'Production', 'production', '{"version":1,"environmentSlug":"production","services":[],"variableGroups":[],"volumes":[]}'
      );
    `);
    await harness.db.insert(schema.environmentSavedStateSnapshot).values({
      id: savedId,
      organizationId,
      environmentId,
      actorId: userId,
      intent: emptySavedIntent,
      volumeDeletionAuthorizations: [],
    });
    await harness.db.insert(schema.environmentDeployment).values({
      id: firstDeploymentId,
      organizationId,
      environmentId,
      savedStateSnapshotId: savedId,
      triggerOrigin: { origin: "manual", actorId: userId },
      dispatchRequestedAt: new Date(),
    });
  });

  it("allows same-run replay without allowing another run to steal the row", async () => {
    expect(
      await runEffect(
        recordInngestRun({
          environmentDeploymentId: firstDeploymentId,
          runId: "run-owner",
        }),
      ),
    ).toBe(true);
    expect(
      await runEffect(
        recordInngestRun({
          environmentDeploymentId: firstDeploymentId,
          runId: "run-owner",
        }),
      ),
    ).toBe(true);
    expect(
      await runEffect(
        recordInngestRun({
          environmentDeploymentId: firstDeploymentId,
          runId: "run-other",
        }),
      ),
    ).toBe(false);

    expect(
      await runEffect(
        beginEnvironmentDeploymentPlanning({
          environmentDeploymentId: firstDeploymentId,
        }),
      ),
    ).toEqual({ state: "started" });
    expect(
      await runEffect(
        ownsDeploymentRun({
          environmentDeploymentId: firstDeploymentId,
          inngestRunId: "run-owner",
        }),
      ),
    ).toBe(true);
    expect(
      await runEffect(
        ownsDeploymentRun({
          environmentDeploymentId: firstDeploymentId,
          inngestRunId: "run-other",
        }),
      ),
    ).toBe(false);
    expect(
      await runEffect(
        recordInngestRun({
          environmentDeploymentId: firstDeploymentId,
          runId: "run-other",
        }),
      ),
    ).toBe(false);
  });

  it("reports contention without taking ownership of the active attempt", async () => {
    await runEffect(
      recordInngestRun({
        environmentDeploymentId: firstDeploymentId,
        runId: "run-active",
      }),
    );
    await runEffect(
      beginEnvironmentDeploymentPlanning({
        environmentDeploymentId: firstDeploymentId,
      }),
    );
    await harness.db.insert(schema.environmentDeployment).values({
      id: secondDeploymentId,
      organizationId,
      environmentId,
      savedStateSnapshotId: savedId,
      triggerOrigin: { origin: "manual", actorId: userId },
      dispatchRequestedAt: new Date(),
    });
    await runEffect(
      recordInngestRun({
        environmentDeploymentId: secondDeploymentId,
        runId: "run-waiting",
      }),
    );

    expect(
      await runEffect(
        beginEnvironmentDeploymentPlanning({
          environmentDeploymentId: secondDeploymentId,
        }),
      ),
    ).toEqual({ state: "blocked" });
    expect(
      await runEffect(
        ownsDeploymentRun({
          environmentDeploymentId: firstDeploymentId,
          inngestRunId: "run-active",
        }),
      ),
    ).toBe(true);
  });

  it("cancels a queued deployment before a run claims it", async () => {
    expect(await runEffect(markDeploymentCancelled(
      { deploymentId: firstDeploymentId }, "Cancelled by user.",
    ))).toBe(true);
    expect(await runEffect(recordInngestRun({
      environmentDeploymentId: firstDeploymentId, runId: "late-run",
    }))).toBe(false);
    const row = await harness.pool.query(
      "select status, cancellation_requested_at, finished_at from environment_deployment where id = $1",
      [firstDeploymentId],
    );
    expect(row.rows[0]).toMatchObject({ status: "cancelled" });
    expect(row.rows[0].cancellation_requested_at).not.toBeNull();
    expect(row.rows[0].finished_at).not.toBeNull();
  });

  it("terminalizes cancellation only for the persisted run owner", async () => {
    await runEffect(
      recordInngestRun({
        environmentDeploymentId: firstDeploymentId,
        runId: "run-owner",
      }),
    );
    await runEffect(
      beginEnvironmentDeploymentPlanning({
        environmentDeploymentId: firstDeploymentId,
      }),
    );

    expect(
      await runEffect(
        markCancelledByInngestRunId("run-other"),
      ),
    ).toBe(false);
    expect(
      await runEffect(
        markCancelledByInngestRunId("run-owner"),
      ),
    ).toBe(true);
    expect(
      await runEffect(
        markCancelledByInngestRunId("run-owner"),
      ),
    ).toBe(false);

    const [deployment] = await harness.db
      .select({
        status: schema.environmentDeployment.status,
        inngestRunId: schema.environmentDeployment.inngestRunId,
        cancellationRequestedAt:
          schema.environmentDeployment.cancellationRequestedAt,
        finishedAt: schema.environmentDeployment.finishedAt,
      })
      .from(schema.environmentDeployment)
      .where(eq(schema.environmentDeployment.id, firstDeploymentId));
    expect(deployment).toEqual({
      status: "cancelled",
      inngestRunId: "run-owner",
      cancellationRequestedAt: expect.any(Date),
      finishedAt: expect.any(Date),
    });
    expect(
      await runEffect(
        ownsDeploymentRun({
          environmentDeploymentId: firstDeploymentId,
          inngestRunId: "run-owner",
        }),
      ),
    ).toBe(false);
  });
});
