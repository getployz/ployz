import type { Client, ClusterTeardown, MachineId } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";
import { Cause, Effect, Exit } from "effect";
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
  destroyClusterActivity,
  completeTeardownAttemptActivity,
  dropTeardownCloudRowsActivity,
  failOwnedTeardownAttemptActivity,
  prepareTeardownAttemptActivity,
  recordTeardownRuntimeEvidenceActivity,
} from "#/modules/runtime/teardown-activities.server";
import type { TeardownOutcome } from "#/modules/runtime/teardown";
import {
  completeTeardownAttempt,
  insertTeardownAttempt,
} from "#/modules/runtime/teardown.repository";
import { dispatchTeardownRequested } from "#/modules/runtime/teardown.server";
import { Validation } from "#/server/public-error";

const encryption = makeSecretEncryption("teardown-test-encryption");
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
            projectName: "app-production",
            cloudName: "acme/app/Production",
          },
        ],
        destroyRuntimeProjects: true,
        revokePairing: false,
        runtimeMembership: "untouched",
      },
    }),
  );
}

type RuntimeCompletionInput = {
  attemptId: string;
  inngestRunId: string;
  status: "completed" | "partial";
  outcome?: TeardownOutcome | null;
  now?: Date;
};

function completeFromRuntimeInput(input: RuntimeCompletionInput) {
  // SAFETY: The test models a JavaScript caller that bypasses TypeScript's input contract.
  return completeTeardownAttempt(
    input as Parameters<typeof completeTeardownAttempt>[0],
  );
}

describe("teardown durable state", () => {
  let harness: GithubPostgresTestHarness;

  function runPromiseDb<A, E>(
    operation: Effect.Effect<A, E, import("#/server/database.server").Database | OrganizationRuntime | SecretEncryption>,
  ) {
    return harness.runEffect(operation.pipe(
      Effect.provideService(SecretEncryption, encryption),
      Effect.provideService(OrganizationRuntime, {
        cancel: () => Effect.void,
        open: () => Effect.die("durable state checks must not open runtime sessions"),
      }),
    ));
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

  it("returns the cluster partial result using only the protected removal credential", async () => {
    const machineId = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" as MachineId;
    const volume = { kind: "docker_volume" as const, id: { machine_id: machineId, name: "data" } };
    const clusterTeardown: ClusterTeardown = {
      destroyed_projects: [],
      machines: {
        successes: [],
        failures: [{ machine_id: machineId, error: { code: "unavailable", message: "machine did not answer", details: null } }],
        omissions: [],
      },
      pairing_revoked: false,
    };
    const retained = "tailcat://protected-removal";
    await harness.pool.query(`
      insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_claim_machine_id,
        founder_machine_id, removal_started_at, removal_endpoints)
      values ($1,$2,$3,$3,now(),$4)
    `, [organizationId, encryption.encrypt("pairing-secret"), machineId, JSON.stringify([
      { machineId, status: "pending", encryptedExpected: encryption.encrypt(retained) },
    ])]);
    const calls: unknown[] = [];
    let closed = 0;
    const ployz = makePloyzLayer({ connect: async (options) => {
      expect(options).toEqual(expect.objectContaining({ connections: [{ tailcat: retained, machine_id: machineId }] }));
      return asTestDouble<Client>()({
        destroyCluster: async (...args: Parameters<Client["destroyCluster"]>) => {
          calls.push(args);
          return clusterTeardown;
        },
        close: async () => { closed += 1; },
      });
    } });
    const result = await runPromiseDb(Effect.scoped(destroyClusterActivity({
      organizationId, confirmDataLoss: [volume],
    })).pipe(Effect.provide(ployz)));
    expect(result).toEqual(clusterTeardown);
    expect(calls).toEqual([[{ confirmed: [volume] }]]);
    expect(closed).toBe(1);
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
          pairingRevocationUnconfirmed: false,
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

  it("rejects completed terminalization without a runtime outcome", async () => {
    const attempt = await insertAttempt(harness);
    await runPromiseDb(
      prepareTeardownAttemptActivity({
        attemptId: attempt.id,
        inngestRunId: "run-no-outcome",
        now: new Date("2026-09-04T05:04:00Z"),
      }),
    );

    const exit = await runPromiseDb(
      completeFromRuntimeInput({
        attemptId: attempt.id,
        inngestRunId: "run-no-outcome",
        status: "completed",
        now: new Date("2026-09-04T05:05:00Z"),
      }).pipe(Effect.exit),
    );
    expect(Exit.isFailure(exit)).toBe(true);
    if (Exit.isFailure(exit)) {
      expect(Cause.squash(exit.cause)).toBeInstanceOf(Validation);
    }

    const state = await harness.pool.query<{
      status: string;
      outcome: unknown;
    }>(
      "select status, outcome from teardown_attempt where id = $1",
      [attempt.id],
    );
    expect(state.rows).toEqual([{ status: "running", outcome: null }]);
  });

  it("preserves recorded runtime evidence through an unknown outcome or cancellation", async () => {
    const failedAttempt = await insertAttempt(harness);
    await runPromiseDb(
      prepareTeardownAttemptActivity({
        attemptId: failedAttempt.id,
        inngestRunId: "run-fail",
        now: new Date("2026-09-04T06:00:00Z"),
      }),
    );
    const failedEvidence = {
      pairingRevocationUnconfirmed: false,
      runtimeMembership: "untouched" as const,
      projectTeardowns: [
        {
          projectName: "app-production",
          outcome: { type: "success" as const, completed: [] },
        },
      ],
    } satisfies TeardownOutcome;
    await runPromiseDb(
      recordTeardownRuntimeEvidenceActivity({
        attemptId: failedAttempt.id,
        inngestRunId: "run-fail",
        outcome: failedEvidence,
        now: new Date("2026-09-04T06:00:30Z"),
      }),
    );
    const stolenEvidence = await runPromiseDb(
      recordTeardownRuntimeEvidenceActivity({
        attemptId: failedAttempt.id,
        inngestRunId: "run-other",
        outcome: failedEvidence,
        now: new Date("2026-09-04T06:00:45Z"),
      }).pipe(Effect.exit),
    );
    expect(Exit.isFailure(stolenEvidence)).toBe(true);
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
    const cancelledEvidence = {
      pairingRevocationUnconfirmed: false,
      runtimeMembership: "untouched" as const,
      projectTeardowns: [
        {
          projectName: "app-production",
          outcome: { type: "success" as const, completed: [] },
        },
      ],
    } satisfies TeardownOutcome;
    await runPromiseDb(
      recordTeardownRuntimeEvidenceActivity({
        attemptId: cancelledAttempt.id,
        inngestRunId: "run-cancel",
        outcome: cancelledEvidence,
        now: new Date("2026-09-04T06:03:30Z"),
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
      outcome: unknown;
      failure_message: string | null;
    }>(
      `select status, inngest_run_id, outcome, failure_message
       from teardown_attempt order by created_at`,
    );
    expect(states.rows).toEqual([
      {
        status: "partial",
        inngest_run_id: "run-fail",
        outcome: failedEvidence,
        failure_message: "Runtime teardown outcome is unknown: retries exhausted",
      },
      {
        status: "cancelled",
        inngest_run_id: "run-cancel",
        outcome: cancelledEvidence,
        failure_message: "Teardown was cancelled.",
      },
    ]);
  });
});
