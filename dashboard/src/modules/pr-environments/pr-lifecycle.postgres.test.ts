import { createHmac } from "node:crypto";
import type { MachineId, ProjectName } from "@ployz/sdk";
import { parseServiceConfig, type ServiceSource } from "@ployz/sdk/config";
import { and, eq, isNotNull } from "drizzle-orm";
import { ConfigProvider, Effect, Layer, Schema } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import * as schema from "#/db/schema";
import { asTestDouble } from "#/lib/test-double";
import { setProjectDefaultEnvironment } from "#/modules/environment-design/workspace-operations.server";
import { createGitServiceSource } from "#/modules/environment-design/services";
import { GithubApi, GithubObservationError, type GithubJsonRequest } from "#/modules/github/github-observation.api";
import { executeProcessGithubPullRequestReceived, executeProcessGithubPushReceived, type GithubIngestionEffectRunner } from "#/modules/github/inngest-ingestion/process";
import { InngestClient, type PloyzStepTools } from "#/modules/inngest/client";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { dropTeardownCloudRowsActivity } from "#/modules/runtime/teardown-activities.server";
import { handleGithubWebhookRequest } from "#/routes/api/github/-webhook.handler";
import { AppConfig } from "#/server/config.server";
import { testConfigEnvironment } from "#/test/config-environment";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import { setPrEnvironmentPlan } from "./plan-operations.server";
import type { PullRequestEffectRunner } from "./pr-lifecycle.server";
import { prDestinations } from "./pr-environment.repository.server";
import { readCollection } from "#/collections/read.server";
import { createServiceVariable, updateServiceVariable } from "#/modules/environment-design/variable-operations.server";
import { withoutSealedCiphertext } from "#/modules/environment-design/saved-intent";
import { branchHostnameSuffix } from "#/modules/branches/branch-plan";
import { goesTo, rowLineage, variableName } from "#/modules/branches/branch-review";
import { approveConditionalSave, giveConditionalSaveValue, withdrawConditionalSave } from "./conditional-save.server";
import { standing } from "./conditional-save";
import { executePostPrCheck } from "./pr-check.inngest";

const organizationId = "00000000-0000-4000-8000-000000001101";
const userId = "00000000-0000-4000-8000-000000001102";
const projectId = "00000000-0000-4000-8000-000000001103";
const stagingId = "00000000-0000-4000-8000-000000001104";
const apiLineage = "00000000-0000-4000-8000-000000001111";
const apiId = "00000000-0000-4000-8000-000000001112";
const dbLineage = "00000000-0000-4000-8000-000000001121";
const dbId = "00000000-0000-4000-8000-000000001122";
const installationId = 17;
const repositoryId = 42;
const secret = "webhook-secret";
const policy = { autoDeploy: false, waitForCi: false, watchPaths: ["api/**"], imageUpdate: { type: "off" } };

function config(slug: string, source: ServiceSource) {
  const { env: _env, mounts: _mounts, ...parsed } = parseServiceConfig({
    version: 2, source, healthcheck: { type: "none" }, restartPolicy: "unless-stopped", privateDns: slug, replicas: 3,
  });
  return parsed;
}

/** staging: api deploys acme/app's main (3 replicas, no deploy on push); db is an image it uses. */
const stagingIntent = {
  version: 1, environmentSlug: "shop-staging",
  services: [
    {
      id: apiId, lineageId: apiLineage, slug: "api",
      config: config("api", createGitServiceSource({ repository: "acme/app", repositoryId, access: { type: "github-installation", installationId } })),
      variables: [], volumeAttachments: [],
    },
    { id: dbId, lineageId: dbLineage, slug: "db", config: config("db", { version: 1, type: "image", image: "postgres:17", credentials: { type: "none" } }), variables: [], volumeAttachments: [] },
  ],
  volumes: [],
};

type PullRequest = { number: number; state: "open" | "closed"; user: { login: string; type: string }; head: { ref: string; sha: string; repo: { id: number } }; base?: { ref: string }; draft: boolean };

type CheckRunBody = { head_sha?: string; conclusion: string; details_url: string; output: { title: string; summary: string } };

type PushDelivery = { ref: string; before: string; after: string; created: boolean; deleted: boolean; forced: boolean };
type PullRequestDelivery = {
  action: string;
  changes: { base: { ref: { from: string } } } | undefined;
  pull_request: PullRequest & { title: string; base: { ref: string }; merged: boolean; merge_commit_sha: null; commits: number };
};

describe("PR Environment lifecycle", () => {
  let harness: PostgresTestHarness;
  const sent: Array<{ name: string }> = [];
  // What GitHub says about each pull request now.
  const pulls = new Map<number, PullRequest>();
  const heads = new Map<string, string>();

  const inngest = new Inngest({ id: "pr-lifecycle" });
  inngest.send = async (input) => {
    sent.push(...(Array.isArray(input) ? input : [input]) as Array<{ name: string }>);
    return { ids: [] };
  };
  // The check runs GitHub has, and whether the installation refuses to write them.
  const checkRuns: Array<{ id: number; headSha: string; body: CheckRunBody }> = [];
  let checksForbidden = false;
  const githubApi = {
    archive: () => Effect.die("unused"),
    json: <S extends Schema.ConstraintDecoder<unknown>>(request: GithubJsonRequest<S>) => {
      if (checksForbidden && request.method) {
        return Effect.fail(new GithubObservationError({ code: "request_failed", operation: request.operation, status: 403, retriable: false, message: "Forbidden" }));
      }
      const body = request.body as CheckRunBody;
      const response = (() => {
        switch (request.operation) {
          case "fetch_pull_request": {
            const pull = pulls.get(Number(request.url.split("/pulls/")[1]));
            return pull && { base: { ref: "main" }, ...pull, title: `Change ${pull.number}` };
          }
          case "resolve_repository": return { id: repositoryId, full_name: "acme/app" };
          case "resolve_branch_head": {
            const ref = decodeURIComponent(request.url.split("/git/ref/")[1] ?? "");
            return { ref: `refs/${ref}`, object: { type: "commit", sha: heads.get(ref.slice("heads/".length)) } };
          }
          case "list_check_runs": {
            const sha = request.url.split("/commits/")[1]?.split("/")[0];
            return { check_runs: checkRuns.filter((run) => run.headSha === sha).map((run) => ({ id: run.id })) };
          }
          case "create_check_run": {
            const run = { id: checkRuns.length + 1, headSha: String(body.head_sha), body };
            checkRuns.push(run);
            return { id: run.id };
          }
          case "update_check_run": {
            const run = checkRuns.find((candidate) => candidate.id === Number(request.url.split("/check-runs/")[1]));
            if (run) run.body = body;
            return { id: run?.id };
          }
          default: return undefined;
        }
      })();
      return Schema.decodeUnknownEffect(request.schema)(response).pipe(Effect.orDie);
    },
  };
  const runtime = {
    cancel: () => Effect.void,
    open: () => Effect.succeed({
      status: "connected" as const,
      connected: asTestDouble<PloyzSession>()({
        dataLossIfProjectDestroyed: (namespace: ProjectName) => Effect.succeed({
          data_loss: [{ kind: "docker_volume" as const, id: { machine_id: "a".repeat(32) as MachineId, name: `${namespace}-data` } }],
          unknown_machines: [],
        }),
      }),
    }),
  };
  const appConfig = () => AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({
    env: { ...testConfigEnvironment(), DATABASE_URL: harness.databaseUrl, GITHUB_APP_WEBHOOK_SECRET: secret },
  }))));
  const runEffect = (<A, E extends Error>(effect: Effect.Effect<A, E, Effect.Services<Parameters<PullRequestEffectRunner>[0]>>) => harness.runEffect(effect.pipe(
    Effect.provide(appConfig()),
    Effect.provideService(GithubApi, githubApi),
    Effect.provideService(InngestClient, inngest),
    Effect.provideService(OrganizationRuntime, runtime),
  ) as never)) as PullRequestEffectRunner & GithubIngestionEffectRunner;
  const run: PloyzStepTools["run"] = async (_id, operation, ...input) => {
    const result = await operation(...input);
    return result === undefined ? null : JSON.parse(JSON.stringify(result));
  };
  const step = { run, sendEvent: async () => ({ ids: [] }) };

  /** Removes and returns the sent events whose names start with `prefix`. */
  function take(prefix: string) {
    const taken = sent.filter((item) => item.name.startsWith(prefix));
    sent.splice(0, sent.length, ...sent.filter((item) => !item.name.startsWith(prefix)));
    return taken;
  }

  /** Runs every requested check post, as Inngest would. */
  async function postChecks() {
    const results = [];
    for (const queued of take("pr-environments/")) results.push((await executePostPrCheck({ event: queued as never, step }, runEffect as never)).posted);
    return results;
  }

  /** A signed delivery through the webhook, then the Inngest function it queues. */
  async function deliver(event: "pull_request" | "push", deliveryId: string, payload: PushDelivery | PullRequestDelivery) {
    const body = JSON.stringify({ installation: { id: installationId }, repository: { id: repositoryId }, ...payload });
    const signature = createHmac("sha256", secret).update(body).digest("hex");
    const response = await harness.runEffect(handleGithubWebhookRequest(new Request("http://localhost/api/github/webhook", {
      method: "POST",
      headers: { "x-hub-signature-256": `sha256=${signature}`, "x-github-event": event, "x-github-delivery": deliveryId },
      body,
    })).pipe(Effect.provide(appConfig()), Effect.provideService(InngestClient, inngest)));
    expect(response.status).toBe(200);
    for (const queued of take("github/")) {
      const input = { event: queued as never, step, runId: `run-${deliveryId}` };
      if (event === "push") await executeProcessGithubPushReceived(input, runEffect);
      else await executeProcessGithubPullRequestReceived(input, runEffect);
    }
    const [row] = await harness.db.select({ outcome: schema.githubWebhookDelivery.outcome }).from(schema.githubWebhookDelivery)
      .where(eq(schema.githubWebhookDelivery.deliveryId, deliveryId));
    return row?.outcome;
  }

  /** GitHub now has the pull request as given, and a delivery for it arrives. */
  function pullRequest(deliveryId: string, action: string, number: number, change: Partial<PullRequest> = {}) {
    const pull: PullRequest = {
      number, state: action === "closed" ? "closed" : "open", user: { login: "maya", type: "User" },
      head: { ref: `feature-${number}`, sha: "c".repeat(40), repo: { id: repositoryId } }, draft: false, ...change,
    };
    pulls.set(number, pull);
    return deliverPullRequest(deliveryId, action, pull);
  }
  function deliverPullRequest(deliveryId: string, action: string, pull: PullRequest) {
    return deliver("pull_request", deliveryId, {
      action,
      // Only an edit of the target Git branch reaches Ployz; which one it was isn't read.
      changes: action === "edited" ? { base: { ref: { from: "main" } } } : undefined,
      pull_request: {
        base: { ref: "main" }, ...pull, title: `Change ${pull.number}`,
        merged: false, merge_commit_sha: null, commits: 1,
      },
    });
  }

  const prEnvironments = () => harness.db.select().from(schema.environmentBranch).where(isNotNull(schema.environmentBranch.prNumber));
  const prEnvironment = async (number: number) => (await prEnvironments()).find((row) => row.prNumber === number);
  const deploymentsOf = (environmentId: string) => harness.db.select().from(schema.environmentDeployment)
    .where(eq(schema.environmentDeployment.environmentId, environmentId));
  const teardownsOf = async (environmentId: string) => (await harness.db.select().from(schema.teardownAttempt))
    .filter((attempt) => attempt.targets.environments.some((target) => target.environmentId === environmentId));
  /** Its teardown finishes: the Environment and its Branch row go. */
  async function finishTeardown(environmentId: string) {
    const [attempt] = await teardownsOf(environmentId);
    if (!attempt) throw new Error("No teardown was admitted.");
    await harness.runEffect(dropTeardownCloudRowsActivity(attempt));
  }
  const setPlan = (change: Partial<typeof schema.prEnvironmentPlan.$inferInsert>) =>
    harness.db.update(schema.prEnvironmentPlan).set(change).where(eq(schema.prEnvironmentPlan.projectId, projectId));

  beforeAll(async () => {
    harness = await startPostgresTestHarness();
  }, 60_000);
  afterAll(async () => {
    await harness?.stop();
  });

  beforeEach(async () => {
    sent.length = 0;
    checkRuns.length = 0;
    checksForbidden = false;
    pulls.clear();
    heads.clear();
    await harness.pool.query(`
      truncate table github_environment_trigger, github_branch_projection, github_webhook_delivery, teardown_attempt, organization, "user" cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
      insert into "user" (id, email, name) values ('${userId}', 'owner@example.com', 'Owner');
      insert into member (id, organization_id, user_id, role, created_at) values (gen_random_uuid(), '${organizationId}', '${userId}', 'owner', now());
      insert into project (id, organization_id, name, slug) values ('${projectId}', '${organizationId}', 'Shop', 'shop');
      insert into environment (id, project_id, organization_id, name, namespace, intent)
        values ('${stagingId}', '${projectId}', '${organizationId}', 'staging', 'shop-staging', '${JSON.stringify(stagingIntent)}');
      update project set default_environment_id = '${stagingId}';
      insert into service_lineage (id, organization_id, project_id, canonical_name, canonical_slug) values
        ('${apiLineage}', '${organizationId}', '${projectId}', 'API', 'api'),
        ('${dbLineage}', '${organizationId}', '${projectId}', 'DB', 'db');
      insert into service (id, project_id, environment_id, organization_id, lineage_id, name, policy) values
        ('${apiId}', '${projectId}', '${stagingId}', '${organizationId}', '${apiLineage}', 'API', '${JSON.stringify(policy)}'),
        ('${dbId}', '${projectId}', '${stagingId}', '${organizationId}', '${dbLineage}', 'DB', '${JSON.stringify(policy)}');
      insert into pr_environment_plan (organization_id, project_id, repository_id, installation_id, repository, enabled,
        start_from_environment_id, picks, enabled_by_user_id)
        values ('${organizationId}', '${projectId}', ${repositoryId}, ${installationId}, 'acme/app', true, '${stagingId}', '{"preset":"only"}', '${userId}');
    `);
  });

  it("opens pr-<number> from the start-from Environment, redeploys it on push, and refuses it as a Default or start-from", async () => {
    // A draft counts; a repeated delivery still leaves one.
    expect(await pullRequest("opened", "opened", 142, { draft: true })).toBe("pull_request_projected");
    expect(await pullRequest("opened-again", "opened", 142, { draft: true })).toBe("pull_request_projected");
    expect(await prEnvironments()).toHaveLength(1);

    const branch = await prEnvironment(142);
    expect(branch).toMatchObject({
      parentEnvironmentId: stagingId, createdByUserId: userId, kept: false,
      prRepositoryId: repositoryId, prRepository: "acme/app", prNumber: 142, prTitle: "Change 142", prAuthor: "maya",
      prHeadBranch: "feature-142", prHeadSha: "c".repeat(40), prTargetBranch: "main",
    });
    const environmentId = branch?.environmentId ?? "";
    const [environment] = await harness.db.select().from(schema.environment).where(eq(schema.environment.id, environmentId));
    expect(environment).toMatchObject({ name: "pr-142", namespace: "shop-pr-142" });
    // Only api is its own: it tracks the head Git branch with one replica, and deploys on push with its watch paths kept.
    expect(environment?.intent.services.map((node) => [node.lineageId, node.config.replicas, (node.config as { source: unknown }).source])).toEqual([
      [apiLineage, 1, expect.objectContaining({ repositoryId, branch: { type: "connected", name: "feature-142" } })],
    ]);
    const services = await harness.db.select().from(schema.service).where(eq(schema.service.environmentId, environmentId));
    expect(services.map((row) => row.policy)).toEqual([{ ...policy, autoDeploy: true }]);
    expect((await deploymentsOf(environmentId)).map((row) => row.triggerOrigin.origin)).toEqual(["manual"]);

    // A push to the head Git branch goes through the Git trigger path.
    heads.set("feature-142", "d".repeat(40));
    await deliver("push", "push-1", {
      ref: "refs/heads/feature-142", before: "c".repeat(40), after: "d".repeat(40), created: false, deleted: false, forced: false,
    });
    // It takes the queued first deployment's place.
    expect((await deploymentsOf(environmentId)).map((row) => row.triggerOrigin.origin)).toEqual(["github"]);
    // Staging tracks main, so it deploys nothing.
    expect(await deploymentsOf(stagingId)).toEqual([]);

    // A later delivery refreshes the recorded head.
    expect(await pullRequest("synchronize", "synchronize", 142, { head: { ref: "feature-142", sha: "d".repeat(40), repo: { id: repositoryId } } }))
      .toBe("pull_request_projected");
    expect((await prEnvironment(142))?.prHeadSha).toBe("d".repeat(40));

    const asDefault = await harness.runEffect(setProjectDefaultEnvironment({ userId }, {
      organizationSlug: "acme", projectSlug: "shop", environmentId,
    }).pipe(Effect.flip));
    expect(asDefault).toMatchObject({ _tag: "Validation", message: "A PR environment can't be the Default Environment." });
    const asStart = await harness.runEffect(setPrEnvironmentPlan({ userId }, {
      organizationSlug: "acme", projectSlug: "shop", repositoryId, enabled: true, startFromEnvironmentId: environmentId,
      picks: { preset: "only" }, setupCommands: [], removeOnClose: true, includeBots: false,
    }).pipe(Effect.flip));
    expect(asStart).toMatchObject({ _tag: "Validation", message: "A PR environment can't be where PR environments start from." });
  });

  it("keeps picks and Then run commands the start-from lacks out of the PR Environment's plan", async () => {
    // Picked while another Environment was the start-from: a lineage staging lacks, and a command for it.
    const gone = "00000000-0000-4000-8000-0000000000ff";
    await setPlan({ picks: { own: [dbLineage, gone] }, setupCommands: [{ lineageId: gone, command: "php artisan migrate" }] });
    expect(await pullRequest("opened", "opened", 142)).toBe("pull_request_projected");
    const branch = await prEnvironment(142);
    expect(branch?.setupCommands).toEqual([]);
    const [environment] = await harness.db.select().from(schema.environment).where(eq(schema.environment.id, branch?.environmentId ?? ""));
    expect(environment?.intent.services.map((node) => node.lineageId).sort()).toEqual([apiLineage, dbLineage].sort());
  });

  it("ignores forks, follows the bots setting, and records a start-from that runs nothing from the repository", async () => {
    expect(await pullRequest("fork", "opened", 7, { head: { ref: "x", sha: "c".repeat(40), repo: { id: 99 } } })).toBe("ignored_fork");
    expect(await pullRequest("bot", "opened", 8, { user: { login: "renovate[bot]", type: "Bot" } })).toBe("ignored_pull_request");
    expect(await prEnvironments()).toEqual([]);

    await setPlan({ includeBots: true });
    expect(await pullRequest("bot-push", "synchronize", 8, { user: { login: "renovate[bot]", type: "Bot" } })).toBe("pull_request_projected");
    expect((await prEnvironments()).map((row) => row.prNumber)).toEqual([8]);

    // Turned off: no new ones.
    await setPlan({ enabled: false });
    expect(await pullRequest("off", "opened", 9)).toBe("ignored_pull_request");

    await setPlan({ enabled: true });
    await harness.db.update(schema.environment).set({ intent: { ...stagingIntent, services: stagingIntent.services.filter((node) => node.lineageId === dbLineage) } as never })
      .where(eq(schema.environment.id, stagingId));
    expect(await pullRequest("nothing", "opened", 10)).toBe("ignored_nothing_from_repository");
    expect((await prEnvironments()).map((row) => row.prNumber)).toEqual([8]);
  });

  it("tears it down when it closes, recreates it on reopen or push, and never undoes a newer state", async () => {
    await pullRequest("opened", "opened", 142);
    const first = (await prEnvironment(142))?.environmentId ?? "";

    expect(await pullRequest("closed", "closed", 142)).toBe("pull_request_projected");
    expect(await teardownsOf(first)).toHaveLength(1);
    // Reopened while the old one is still going: the delivery fails and Inngest retries it.
    await expect(pullRequest("reopened-early", "reopened", 142)).rejects.toThrow("still being torn down");
    await finishTeardown(first);
    expect(await pullRequest("reopened", "reopened", 142)).toBe("pull_request_projected");
    const second = (await prEnvironment(142))?.environmentId ?? "";
    expect(second).not.toBe(first);

    // Closed by hand: the next push to the pull request brings it back.
    await harness.db.delete(schema.environment).where(eq(schema.environment.id, second));
    expect(await pullRequest("synchronize", "synchronize", 142)).toBe("pull_request_projected");
    const third = (await prEnvironment(142))?.environmentId ?? "";
    expect(third).not.toBe(second);

    // With "remove its environment" off, closing leaves it.
    await setPlan({ removeOnClose: false });
    await pullRequest("closed-kept", "closed", 142);
    expect(await teardownsOf(third)).toEqual([]);
    expect(await prEnvironment(142)).toBeDefined();

    // An opened delivery processed after the pull request closed creates nothing.
    const late: PullRequest = { number: 143, state: "closed", user: { login: "maya", type: "User" }, head: { ref: "late", sha: "c".repeat(40), repo: { id: repositoryId } }, draft: false };
    pulls.set(143, late);
    expect(await deliverPullRequest("late-opened", "opened", { ...late, state: "open" })).toBe("ignored_pull_request");
    expect(await prEnvironment(143)).toBeUndefined();
    expect(await harness.db.select().from(schema.environmentBranch)
      .where(and(eq(schema.environmentBranch.projectId, projectId), eq(schema.environmentBranch.prNumber, 143)))).toEqual([]);
  });

  it("follows the pull request's target Git branch, and its Destinations with it", async () => {
    // staging tracks main; dev tracks dev. Each by its latest Saved State.
    const devId = "00000000-0000-4000-8000-000000001105";
    const devIntent = { ...stagingIntent, environmentSlug: "shop-dev", services: stagingIntent.services.map((node) => node.lineageId === apiLineage
      ? { ...node, config: { ...node.config, source: { ...node.config.source, branch: { type: "connected", name: "dev" } } } } : node) };
    await harness.db.insert(schema.environment).values({ id: devId, projectId, organizationId, name: "dev", namespace: "shop-dev", intent: devIntent as never });
    await harness.db.insert(schema.environmentSavedStateSnapshot).values([stagingId, devId].map((environmentId) => ({
      organizationId, environmentId, actorId: userId, intent: (environmentId === devId ? devIntent : stagingIntent) as never, volumeDeletionAuthorizations: [],
    })));

    await pullRequest("opened", "opened", 142);
    const environmentId = (await prEnvironment(142))?.environmentId ?? "";
    const destinationsNow = () => harness.runEffect(prDestinations(environmentId));
    expect(await destinationsNow()).toEqual([stagingId]);

    expect(await pullRequest("edited", "edited", 142, { base: { ref: "dev" } })).toBe("pull_request_projected");
    expect((await prEnvironment(142))?.prTargetBranch).toBe("dev");
    expect(await destinationsNow()).toEqual([devId]);

    // Nothing deploys release: no Destinations.
    await pullRequest("edited-again", "edited", 142, { base: { ref: "release" } });
    expect(await destinationsNow()).toEqual([]);
  });

  describe("approving for a Destination", () => {
    const organizationSlug = "acme";
    const environmentOf = async (id: string) => (await harness.db.select().from(schema.environment).where(eq(schema.environment.id, id)))[0];
    const scope = async (environmentId: string) => {
      const row = await environmentOf(environmentId);
      const serviceId = row?.intent.services.find((node) => node.lineageId === apiLineage)?.id ?? "";
      return { organizationSlug, environmentId, revision: row?.revision ?? "", serviceId };
    };

    /** The review of what goes to staging, computed as the browser does from Org Store rows. */
    async function review(prId: string) {
      const branch = await prEnvironment(142);
      const [pr, staging] = [await environmentOf(prId), await environmentOf(stagingId)];
      if (!branch || !pr || !staging) throw new Error("missing rows");
      return goesTo({
        base: withoutSealedCiphertext(branch.base), kept: false, branch: pr.intent, parent: staging.intent, parentApplied: null,
        hostnames: { branch: branchHostnameSuffix("shop", pr.namespace, true), parent: branchHostnameSuffix("shop", staging.namespace, false) },
      });
    }
    const approve = async (prId: string, picks: (keys: string[]) => Array<{ key: string; option?: "from" | "new"; value: string }>) => {
      const { rows, review: string } = await review(prId);
      return runEffect(approveConditionalSave({ userId }, {
        organizationSlug, prEnvironmentId: prId, destinationEnvironmentId: stagingId, review: string, picks: picks(rows.map((row) => row.key)),
      }));
    };
    const saves = () => harness.db.select().from(schema.conditionalSave);
    const stands = async (prId: string) => {
      const [save] = await saves();
      const [pr, branch] = [await environmentOf(prId), await prEnvironment(142)];
      return save !== undefined && standing(save, pr && { id: pr.id, revision: pr.revision, targetBranch: branch?.prTargetBranch ?? null });
    };

    beforeEach(async () => {
      await harness.db.insert(schema.environmentSavedStateSnapshot).values({
        organizationId, environmentId: stagingId, actorId: userId, intent: stagingIntent, volumeDeletionAuthorizations: [],
      });
      await pullRequest("opened", "opened", 142);
    });

    it("holds the ticked rows on the Destination, sealed, until the PR Environment's settings change", async () => {
      const prId = (await prEnvironment(142))?.environmentId ?? "";
      await runEffect(createServiceVariable({ userId }, { ...(await scope(prId)), key: "FLAG", description: null, exported: false, value: { type: "plain", value: "on" } }));
      await runEffect(createServiceVariable({ userId }, { ...(await scope(prId)), key: "STRIPE_KEY", description: null, exported: false, value: { type: "sealed", value: "sk_test_pr" } }));
      const { rows } = await review(prId);
      const flag = rows.find((row) => variableName(row) === "FLAG")?.key ?? "";
      const stripe = rows.find((row) => variableName(row) === "STRIPE_KEY")?.key ?? "";
      expect(rowLineage({ key: stripe })).toBe(apiLineage);

      // A stale review is refused.
      const stale = await runEffect(approveConditionalSave({ userId }, {
        organizationSlug, prEnvironmentId: prId, destinationEnvironmentId: stagingId, review: "old", picks: [{ key: flag, value: "" }],
      }).pipe(Effect.as(null), Effect.catch(Effect.succeed)));
      expect(stale).toMatchObject({ _tag: "Conflict", message: "Changes moved after this review. Review them again." });

      // The browser's review string is the server's; a secret asks for staging's value, which can come later.
      await approve(prId, () => [{ key: flag, option: "from", value: "" }, { key: stripe, option: "new", value: "" }]);
      const [save] = await saves();
      expect(save).toMatchObject({
        prEnvironmentId: prId, prNumber: 142, repositoryId, destinationEnvironmentId: stagingId, targetBranch: "main", approvedByUserId: userId,
        workingRevision: (await environmentOf(prId))?.revision,
      });
      expect(save?.rows.map((row) => [row.row.key, row.option, row.missing])).toEqual([[flag, "from", false], [stripe, "new", true]]);
      expect(Object.keys(save?.approvedAgainst ?? {})).toEqual([flag, stripe]);
      expect(save?.landing.identities.services.map((row) => row.lineageId)).toEqual([apiLineage]);
      expect(await stands(prId)).toBe(true);

      // A value given after approval is sealed and stored without withdrawing it; it never reaches the browser.
      await runEffect(giveConditionalSaveValue({ userId }, { organizationSlug, prEnvironmentId: prId, destinationEnvironmentId: stagingId, key: stripe, value: "sk_live_staging" }));
      const [given] = await saves();
      expect(JSON.stringify(given?.picks)).not.toContain("sk_live_staging");
      expect(given?.picks.find((pick) => pick.key === stripe)?.choice).toMatchObject({ option: "new", value: { valueFingerprint: expect.any(String) } });
      expect(given?.rows.find((row) => row.row.key === stripe)?.missing).toBe(false);
      expect(await stands(prId)).toBe(true);
      const read = await harness.runEffect(readCollection({ userId }, { table: "conditional_save", userId, organizationSlug }));
      expect(read.rows).toEqual([expect.objectContaining({ id: given?.id, approvedBy: "Owner" })]);
      expect(JSON.stringify(read.rows)).not.toMatch(/encryptedValue|landing|picks|approvedAgainst/u);

      // An edit in the Destination and a new commit leave it standing.
      await runEffect(createServiceVariable({ userId }, { ...(await scope(stagingId)), key: "OTHER", description: null, exported: false, value: { type: "plain", value: "x" } }));
      heads.set("feature-142", "d".repeat(40));
      await deliver("push", "push-1", { ref: "refs/heads/feature-142", before: "c".repeat(40), after: "d".repeat(40), created: false, deleted: false, forced: false });
      await pullRequest("synchronize", "synchronize", 142, { head: { ref: "feature-142", sha: "d".repeat(40), repo: { id: repositoryId } } });
      expect(await stands(prId)).toBe(true);

      // A settings change on the PR Environment withdraws it.
      const flagId = (await environmentOf(prId))?.intent.services.find((node) => node.lineageId === apiLineage)?.variables.find((variable) => variable.key === "FLAG")?.id ?? "";
      await runEffect(updateServiceVariable({ userId }, { ...(await scope(prId)), variableId: flagId, key: "FLAG", description: null, exported: false, value: { type: "plain", value: "off" } }));
      expect(await stands(prId)).toBe(false);
    });

    it("is withdrawn by Undo, a new target Git branch, and the PR Environment closing", async () => {
      const prId = (await prEnvironment(142))?.environmentId ?? "";
      await runEffect(createServiceVariable({ userId }, { ...(await scope(prId)), key: "FLAG", description: null, exported: false, value: { type: "plain", value: "on" } }));
      const tickAll = (keys: string[]) => keys.map((key) => ({ key, option: "from" as const, value: "" }));

      await approve(prId, tickAll);
      await runEffect(withdrawConditionalSave({ userId }, { organizationSlug, prEnvironmentId: prId, destinationEnvironmentId: stagingId }));
      expect(await saves()).toEqual([]);

      await approve(prId, tickAll);
      await pullRequest("edited", "edited", 142, { base: { ref: "dev" } });
      expect(await saves()).toEqual([]);

      await pullRequest("edited-back", "edited", 142, { base: { ref: "main" } });
      await approve(prId, tickAll);
      await harness.db.delete(schema.environment).where(eq(schema.environment.id, prId));
      expect(await saves()).toEqual([]);
    });

    it("posts the Ployz · ready to merge check on the head commit, and updates it as the PR Environment changes", async () => {
      const prId = (await prEnvironment(142))?.environmentId ?? "";
      const [first, second] = ["c".repeat(40), "d".repeat(40)];
      const runOn = (sha: string) => {
        const runs = checkRuns.filter((run) => run.headSha === sha);
        expect(runs).toHaveLength(1);
        const body = runs[0]?.body;
        return { conclusion: body?.conclusion, reason: body?.output.title, detailsUrl: body?.details_url, summary: body?.output.summary };
      };
      // Its web addresses: api's managed hostname on the Organization's Cluster Domain.
      await harness.pool.query(`insert into organization_cluster_domain (organization_id, endpoint, name, encrypted_token, reserved_at, lease_renewed_at)
        values ('${organizationId}', 'https://dns.test', 'acme.ployz.test', '{}', now(), now())`);
      const pr = await environmentOf(prId);
      await harness.db.update(schema.environment).set({ intent: { ...pr?.intent, services: pr?.intent.services.map((node) => ({
        ...node, config: { ...node.config, managedHostnames: [{ prefix: "api-pr-142", targetPort: 3000 }] },
      })) } as never }).where(eq(schema.environment.id, prId));

      // Opened: nothing here staging lacks yet.
      expect(await postChecks()).toEqual(["posted"]);
      expect(runOn(first)).toEqual({
        conclusion: "success", reason: "Nothing here that staging doesn’t have",
        detailsUrl: expect.stringMatching(/\/cloud\/acme\/shop\/shop-pr-142\/review$/u),
        summary: expect.stringContaining("- https://api-pr-142.acme.ployz.test"),
      });

      // A setting edited on the PR Environment updates the same check run.
      await runEffect(createServiceVariable({ userId }, { ...(await scope(prId)), key: "STRIPE_KEY", description: null, exported: false, value: { type: "sealed", value: "sk_test_pr" } }));
      await postChecks();
      expect(runOn(first)).toMatchObject({ conclusion: "action_required", reason: "Review and approve 1 change for staging" });

      // A new head gets its own.
      await pullRequest("synchronize", "synchronize", 142, { head: { ref: "feature-142", sha: second, repo: { id: repositoryId } } });
      expect(await postChecks()).toEqual(["posted"]);
      expect(runOn(second)).toMatchObject({ conclusion: "action_required", reason: "Review and approve 1 change for staging" });

      // Approved without staging's value, then given it.
      await approve(prId, (keys) => keys.map((key) => ({ key, option: "new" as const, value: "" })));
      await postChecks();
      expect(runOn(second)).toMatchObject({ conclusion: "action_required", reason: "STRIPE_KEY needs a value for staging" });
      const stripe = (await saves())[0]?.rows[0]?.row.key ?? "";
      await runEffect(giveConditionalSaveValue({ userId }, { organizationSlug, prEnvironmentId: prId, destinationEnvironmentId: stagingId, key: stripe, value: "sk_live" }));
      await postChecks();
      expect(runOn(second)).toMatchObject({ conclusion: "success", reason: "1 change approved for staging by Owner" });

      // Undo.
      await runEffect(withdrawConditionalSave({ userId }, { organizationSlug, prEnvironmentId: prId, destinationEnvironmentId: stagingId }));
      await postChecks();
      expect(runOn(second)).toMatchObject({ conclusion: "action_required", reason: "Review and approve 1 change for staging" });

      // Without Checks: write nothing is posted, and nothing fails.
      checksForbidden = true;
      await approve(prId, (keys) => keys.map((key) => ({ key, option: "new" as const, value: "" })));
      expect(await postChecks()).toEqual(["forbidden"]);
      expect(runOn(second)).toMatchObject({ reason: "Review and approve 1 change for staging" });
      expect(checkRuns).toHaveLength(2);

      // A pull request without a PR Environment gets none.
      checksForbidden = false;
      await setPlan({ enabled: false });
      expect(await pullRequest("other", "opened", 9, { head: { ref: "other", sha: "e".repeat(40), repo: { id: repositoryId } } })).toBe("ignored_pull_request");
      expect(await postChecks()).toEqual([]);
      expect(checkRuns).toHaveLength(2);
    });
  });
});
