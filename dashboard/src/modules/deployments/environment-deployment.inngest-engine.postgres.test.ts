import { InngestTestEngine, mockCtx } from "@inngest/test";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import * as schema from "#/db/schema";
import {
  type GithubPostgresTestHarness,
  startGithubPostgresTestHarness,
} from "#/modules/github/github-ingestion.postgres-test-harness";
import { Database } from "#/server/database.server";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";
import {
  makeSecretEncryption,
  SecretEncryption,
} from "#/utils/encrypted-secret.server";
import {
  createMarkCancelledRowBackedWorkflow,
  createProcessEnvironmentDeployment,
} from "./environment-deployment.inngest";

const organizationId = "00000000-0000-4000-8000-000000000701";
const userId = "00000000-0000-4000-8000-000000000702";
const projectId = "00000000-0000-4000-8000-000000000703";
const environmentId = "00000000-0000-4000-8000-000000000704";
const savedId = "00000000-0000-4000-8000-000000000705";
const activeDeploymentId = "00000000-0000-4000-8000-000000000706";
const targetDeploymentId = "00000000-0000-4000-8000-000000000707";
const targetRunId = "deployment-smoke-run";
const encryption = makeSecretEncryption("test-encryption-secret");

const emptySavedIntent = {
  version: 1 as const,
  environmentSlug: "production",
  services: [],
  variableGroups: [],
  volumes: [],
};

describe("deployment Inngest durable smoke", () => {
  let harness: GithubPostgresTestHarness;

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
      values ('${organizationId}', 'Durable smoke', 'durable-smoke');
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
    await harness.db.insert(schema.environmentDeployment).values([
      {
        id: activeDeploymentId,
        organizationId,
        environmentId,
        savedStateSnapshotId: savedId,
        triggerOrigin: { origin: "manual", actorId: userId },
        dispatchRequestedAt: new Date(),
        status: "planning",
        inngestRunId: "active-deployment-run",
      },
      {
        id: targetDeploymentId,
        organizationId,
        environmentId,
        savedStateSnapshotId: savedId,
        triggerOrigin: { origin: "manual", actorId: userId },
        dispatchRequestedAt: new Date(),
      },
    ]);
  });

  it("resumes after contention and terminalizes cancellation in PostgreSQL", async () => {
    const runEffect = makeInngestEffectRunner(
      <A, E>(operation: Effect.Effect<A, E, Database | SecretEncryption>) =>
        harness.runEffect(
          operation.pipe(Effect.provideService(SecretEncryption, encryption)),
        ),
    ) as typeof runInngestEffect;
    const inngest = new Inngest({ id: "durable-smoke" });
    let interrupted = false;

    const resumed = await new InngestTestEngine({
      function: createProcessEnvironmentDeployment(inngest, runEffect),
      events: [
        {
          name: "environment/deploy.requested",
          data: {
            environmentDeploymentId: targetDeploymentId,
            environmentId,
          },
        },
      ],
      steps: [
        {
          id: "wait-for-active-deployment",
          handler: async () => {
            interrupted = true;
            await harness.db
              .update(schema.environmentDeployment)
              .set({ status: "applied", finishedAt: new Date() })
              .where(eq(schema.environmentDeployment.id, activeDeploymentId));
          },
        },
      ],
      transformCtx: (context) => ({
        ...mockCtx(context),
        runId: targetRunId,
      }),
    }).executeStep("mark-deployment-deploying");

    expect(interrupted).toBe(true);
    expect(resumed.result).toBe(true);
    const [planning] = await harness.db
      .select({
        status: schema.environmentDeployment.status,
        inngestRunId: schema.environmentDeployment.inngestRunId,
      })
      .from(schema.environmentDeployment)
      .where(eq(schema.environmentDeployment.id, targetDeploymentId));
    expect(planning).toEqual({ status: "deploying", inngestRunId: targetRunId });

    const cancellation = await new InngestTestEngine({
      function: createMarkCancelledRowBackedWorkflow(inngest, runEffect),
      events: [
        {
          name: "inngest/function.cancelled",
          data: {
            function_id: "process-environment-deployment",
            run_id: targetRunId,
          },
        },
      ],
    }).execute();

    expect(cancellation.error).toBeUndefined();
    expect(cancellation.result).toEqual({
      functionId: "process-environment-deployment",
      runId: targetRunId,
      marked: true,
    });
    const [terminal] = await harness.db
      .select({
        status: schema.environmentDeployment.status,
        inngestRunId: schema.environmentDeployment.inngestRunId,
        cancellationRequestedAt:
          schema.environmentDeployment.cancellationRequestedAt,
        finishedAt: schema.environmentDeployment.finishedAt,
      })
      .from(schema.environmentDeployment)
      .where(eq(schema.environmentDeployment.id, targetDeploymentId));
    expect(terminal).toEqual({
      status: "cancelled",
      inngestRunId: targetRunId,
      cancellationRequestedAt: expect.any(Date),
      finishedAt: expect.any(Date),
    });
  });
});
