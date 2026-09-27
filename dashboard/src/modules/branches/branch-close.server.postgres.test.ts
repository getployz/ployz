import type { MachineId, ProjectName } from "@ployz/sdk";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { InngestClient } from "#/modules/inngest/client";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { dropTeardownCloudRowsActivity } from "#/modules/runtime/teardown-activities.server";
import { confirmTeardown, loadTeardownDataLoss } from "#/modules/runtime/teardown.server";
import { Conflict } from "#/server/public-error";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import { closeBranch, sweepIdleBranches } from "./branch-close.server";

const organizationId = "00000000-0000-4000-8000-000000000901";
const creatorId = "00000000-0000-4000-8000-000000000902";
const userId = "00000000-0000-4000-8000-000000000903";
const projectId = "00000000-0000-4000-8000-000000000904";
const productionId = "00000000-0000-4000-8000-000000000905";
const fixWebId = "00000000-0000-4000-8000-000000000906";
const tryCacheId = "00000000-0000-4000-8000-000000000907";
const stagingId = "00000000-0000-4000-8000-000000000908";
const fixStagingId = "00000000-0000-4000-8000-000000000909";

// The runtime reports one volume per namespace it would destroy.
const volumeOf = (namespace: string) => ({
  kind: "docker_volume" as const,
  id: { machine_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" as MachineId, name: `${namespace}-data` },
});

describe("closing a Branch", () => {
  let harness: PostgresTestHarness;
  let sent: unknown[];

  function run<A, E>(operation: Effect.Effect<A, E, never>) {
    return harness.runEffect(operation);
  }

  function provide<A, E, R>(operation: Effect.Effect<A, E, R>, unreachable?: string) {
    const inngest = new Inngest({ id: "branch-close-postgres" });
    inngest.send = async (event) => {
      sent.push(event);
      return { ids: [] };
    };
    return operation.pipe(
      Effect.scoped,
      Effect.provideService(InngestClient, inngest),
      Effect.provideService(OrganizationRuntime, {
        cancel: () => Effect.void,
        open: () => Effect.succeed({
          status: "connected" as const,
          connected: asTestDouble<PloyzSession>()({
            dataLossIfProjectDestroyed: (namespace: ProjectName) => namespace === unreachable
              ? Effect.fail(new Error("The runtime is unreachable."))
              : Effect.succeed({ data_loss: [volumeOf(namespace)], unknown_machines: [] }),
          }),
        }),
      }),
    ) as Effect.Effect<A, E, never>;
  }

  async function attemptRows() {
    return (await harness.pool.query<{ requested_by_user_id: string; close_reason: string | null; confirm_data_loss: unknown; targets: { environments: { environmentId: string }[] } }>(
      "select requested_by_user_id, close_reason, confirm_data_loss, targets from teardown_attempt",
    )).rows;
  }

  beforeAll(async () => {
    harness = await startPostgresTestHarness();
  }, 60_000);

  afterAll(async () => {
    await harness.stop();
  });

  beforeEach(async () => {
    sent = [];
    const environment = (id: string, name: string) =>
      `('${id}', '${projectId}', '${organizationId}', '${name}', 'app-${name}', '{"version":1,"environmentSlug":"app-${name}","services":[],"volumes":[]}')`;
    const branch = (id: string, parentId: string) =>
      `('${id}', '${organizationId}', '${projectId}', '${parentId}', '{}', '${creatorId}')`;
    await harness.pool.query(`
      truncate table organization, "user", teardown_attempt cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
      insert into "user" (id, email, name) values ('${creatorId}', 'creator@example.com', 'Creator'), ('${userId}', 'user@example.com', 'User');
      insert into member (id, organization_id, user_id, role, created_at)
      values (gen_random_uuid(), '${organizationId}', '${creatorId}', 'member', now()), (gen_random_uuid(), '${organizationId}', '${userId}', 'owner', now());
      insert into project (id, organization_id, name, slug) values ('${projectId}', '${organizationId}', 'App', 'app');
      insert into environment (id, project_id, organization_id, name, namespace, intent) values
        ${environment(productionId, "production")}, ${environment(fixWebId, "fix-web")}, ${environment(tryCacheId, "try-cache")},
        ${environment(stagingId, "staging")}, ${environment(fixStagingId, "fix-staging")};
      update project set default_environment_id = '${productionId}';
      insert into environment_branch (environment_id, organization_id, project_id, parent_environment_id, base, created_by_user_id) values
        ${branch(fixWebId, productionId)}, ${branch(tryCacheId, fixWebId)}, ${branch(fixStagingId, stagingId)};
    `);
  });

  it("closes its Branches first, confirms the runtime's report and records the creator and reason", async () => {
    const attempt = await run(provide(closeBranch({ environmentId: fixWebId, reason: "merged" })));

    expect(attempt.targets.environments.map((target) => target.environmentId)).toEqual([tryCacheId, fixWebId]);
    expect(await attemptRows()).toEqual([expect.objectContaining({
      requested_by_user_id: creatorId,
      close_reason: "merged",
      confirm_data_loss: [volumeOf("app-try-cache"), volumeOf("app-fix-web")],
    })]);
    expect(sent).toHaveLength(1);

    // Both go in one statement, so the Branch never outlives its Parent; production and its row stay.
    await run(provide(dropTeardownCloudRowsActivity(attempt)));
    const environments = await harness.pool.query<{ name: string }>("select name from environment order by name");
    expect(environments.rows.map((row) => row.name)).toEqual(["fix-staging", "production", "staging"]);
    expect((await harness.pool.query("select * from environment_branch")).rowCount).toBe(1);
  });

  it("refuses the Default Environment, whoever asks, even as a Branch's descendant", async () => {
    const byUser = await run(provide(confirmTeardown({ userId }, {
      organizationSlug: "acme", scope: "environment", environmentId: productionId, identities: [],
    }).pipe(Effect.flip)));
    expect(byUser).toBeInstanceOf(Conflict);
    expect(byUser.message).toBe("production is the Default Environment. Choose another Default Environment first.");

    await harness.pool.query(`update project set default_environment_id = '${tryCacheId}'`);
    const bySystem = await run(provide(closeBranch({ environmentId: fixWebId, reason: "idle" }).pipe(Effect.flip)));
    expect(bySystem).toBeInstanceOf(Conflict);
    expect(bySystem.message).toContain("try-cache is the Default Environment");
    expect(await attemptRows()).toEqual([]);
  });

  it("tears down a root with open Branches by listing and closing them first; a person's Branch close reads manual", async () => {
    const dataLoss = await run(provide(loadTeardownDataLoss({ userId }, {
      organizationSlug: "acme", scope: "environment", environmentId: stagingId,
    })));
    expect(dataLoss.rust).toEqual([volumeOf("app-fix-staging"), volumeOf("app-staging")]);

    const root = await run(provide(confirmTeardown({ userId }, {
      organizationSlug: "acme", scope: "environment", environmentId: stagingId, identities: dataLoss.rust,
    })));
    expect(root.targets.environments.map((target) => target.environmentId)).toEqual([fixStagingId, stagingId]);
    expect(root.closeReason).toBeNull();

    const manual = await run(provide(confirmTeardown({ userId }, {
      organizationSlug: "acme", scope: "environment", environmentId: tryCacheId, identities: [],
    })));
    expect(manual).toMatchObject({ requestedByUserId: userId, closeReason: "manual" });
  });

  it("sweeps idle Branches closed as the system; a failed close doesn't stop the others", async () => {
    // Everything but production last deployed 8 days ago; fix-web still has try-cache open.
    for (const id of [fixWebId, tryCacheId, fixStagingId]) {
      await harness.pool.query(`
        with snapshot as (
          insert into environment_saved_state_snapshot (organization_id, environment_id, actor_id, intent, volume_deletion_authorizations)
          values ($1, $2, $3, '{}', '[]') returning id
        )
        insert into environment_deployment (organization_id, environment_id, trigger_origin, saved_state_snapshot_id, created_at)
        select $1, $2, '{}', id, now() - interval '8 days' from snapshot`, [organizationId, id, creatorId]);
    }

    const closed = await run(provide(sweepIdleBranches(new Date()), "app-try-cache"));

    expect(closed).toEqual([fixStagingId]);
    expect(await attemptRows()).toEqual([expect.objectContaining({
      requested_by_user_id: creatorId,
      close_reason: "idle",
      targets: expect.objectContaining({ environments: [expect.objectContaining({ environmentId: fixStagingId })] }),
    })]);
  });
});
