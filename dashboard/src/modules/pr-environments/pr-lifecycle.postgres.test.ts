import { createHmac } from "node:crypto";
import type { MachineId, ProjectName } from "@ployz/sdk";
import { parseServiceConfig, type ServiceSource } from "@ployz/sdk/config";
import { and, eq } from "drizzle-orm";
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
import { dropTeardownCloudRowsActivity, finishShutdownActivity } from "#/modules/runtime/teardown-activities.server";
import { completeTeardownAttempt } from "#/modules/runtime/teardown.repository";
import { handleGithubWebhookRequest } from "#/routes/api/github/-webhook.handler";
import { AppConfig } from "#/server/config.server";
import { testConfigEnvironment } from "#/test/config-environment";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import { setPrEnvironmentPlan } from "./plan-operations.server";
import type { PullRequestEffectRunner } from "./pr-lifecycle.server";
import { prDestinations } from "./pr-environment.repository.server";
import { readCollection } from "#/collections/read.server";
import { createServiceVariable, updateServiceVariable } from "#/modules/environment-design/variable-operations.server";
import { withoutSealedCiphertext, type SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { branchHostnameSuffix } from "#/modules/branches/branch-plan";
import { goesTo, presentRow, rowLineage, variableName } from "#/modules/branches/branch-review";
import { closePrEnvironment } from "#/modules/branches/branch-close.server";
import { saveConditionalSave, withdrawConditionalSave } from "./conditional-save.server";
import { shutDownPrEnvironment, startPrEnvironment } from "./off.server";
import { takePullRequestValue } from "./land.server";
import { standing } from "./conditional-save";
import { executePostPrCheck } from "./pr-check.inngest";
import { postPrCheck } from "./pr-check.server";
import { createService } from "#/modules/environment-design/service-operations.server";
import { createImageServiceSource } from "#/modules/environment-design/services";
import { saveBranch } from "#/modules/branches/branch-save.server";
import { resumeGithubWaitingTriggers } from "#/modules/github/github-ingestion.branch.repository";

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

type PullRequest = {
  number: number; state: "open" | "closed"; user: { login: string; type: string }; head: { ref: string; sha: string; repo: { id: number } };
  base?: { ref: string }; draft: boolean; merged?: boolean; merge_commit_sha?: string | null;
};

type CheckRunBody = { head_sha?: string; external_id: string; conclusion: string; details_url: string; output: { title: string; summary: string } };

type PushDelivery = { ref: string; before: string; after: string; created: boolean; deleted: boolean; forced: boolean };
type PullRequestDelivery = {
  action: string;
  changes: { base: { ref: { from: string } } } | undefined;
  pull_request: PullRequest & { title: string; base: { ref: string }; merged: boolean; merge_commit_sha: string | null; commits: number };
};

describe("PR Environment lifecycle", () => {
  let harness: PostgresTestHarness;
  const sent: Array<{ name: string }> = [];
  // What GitHub says about each pull request now.
  const pulls = new Map<number, PullRequest>();
  const heads = new Map<string, string>();
  // main's commits in order, and the paths each changed.
  const history: string[] = [];
  const changed = new Map<string, string[]>();

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
            return pull && { base: { ref: "main" }, merged: false, merge_commit_sha: null, commits: 1, ...pull, title: `Change ${pull.number}` };
          }
          case "resolve_repository": return { id: repositoryId, full_name: "acme/app" };
          case "resolve_branch_head": {
            const ref = decodeURIComponent(request.url.split("/git/ref/")[1] ?? "");
            return { ref: `refs/${ref}`, object: { type: "commit", sha: heads.get(ref.slice("heads/".length)) } };
          }
          case "compare_commits": {
            const [base = "", head = ""] = request.url.split("/compare/")[1]?.split("...") ?? [];
            const ahead = history.includes(base) && history.indexOf(head) > history.indexOf(base);
            return { status: ahead ? "ahead" : "diverged", base_commit: { sha: base }, files: (changed.get(head) ?? []).map((filename) => ({ filename })) };
          }
          case "list_check_runs": {
            const sha = request.url.split("/commits/")[1]?.split("/")[0];
            const runs = checkRuns.filter((run) => run.headSha === sha);
            // As GitHub does: only the newest run of a name unless asked for all.
            return { check_runs: (request.url.includes("filter=all") ? runs : runs.slice(-1)).map((run) => ({ id: run.id, external_id: run.body.external_id })) };
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
  // Whether the runtime fails to answer, as when it's unreachable.
  let runtimeFails = false;
  const runtime = {
    cancel: () => Effect.void,
    open: () => Effect.succeed({
      status: "connected" as const,
      connected: asTestDouble<PloyzSession>()({
        dataLossIfProjectDestroyed: (namespace: ProjectName) => runtimeFails ? Effect.die("The runtime did not answer.") : Effect.succeed({
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
  /** GitHub now has the pull request changed as given, before any delivery says so. */
  function mergedOnGithub(number: number, change: Partial<PullRequest>) {
    const pull = pulls.get(number);
    if (pull) pulls.set(number, { ...pull, ...change });
  }
  function deliverPullRequest(deliveryId: string, action: string, pull: PullRequest) {
    return deliver("pull_request", deliveryId, {
      action,
      // Only an edit of the title or target Git branch reaches Ployz; what changed is read from GitHub.
      changes: action === "edited" ? { base: { ref: { from: "main" } } } : undefined,
      pull_request: {
        base: { ref: "main" }, ...pull, title: `Change ${pull.number}`,
        merged: pull.merged ?? false, merge_commit_sha: pull.merge_commit_sha ?? null, commits: 1,
      },
    });
  }

  const prEnvironments = async () => (await harness.db.select().from(schema.environmentBranch)
    .innerJoin(schema.prEnvironment, eq(schema.prEnvironment.environmentId, schema.environmentBranch.environmentId)))
    .map((row) => ({ ...row.environment_branch, ...row.pr_environment }));
  const prEnvironment = async (number: number) => {
    const rows = (await prEnvironments()).filter((row) => row.number === number);
    return rows.find((row) => !row.retired) ?? rows[0];
  };
  const deploymentsOf = (environmentId: string) => harness.db.select().from(schema.environmentDeployment)
    .where(eq(schema.environmentDeployment.environmentId, environmentId));
  const teardownsOf = async (environmentId: string) => (await harness.db.select().from(schema.teardownAttempt))
    .filter((attempt) => attempt.targets.environments.some((target) => target.environmentId === environmentId));
  /** Its teardown finishes: the Environment and its Branch row go. */
  async function finishTeardown(environmentId: string) {
    const [attempt] = await teardownsOf(environmentId);
    if (!attempt) throw new Error("No teardown was admitted.");
    await runEffect(dropTeardownCloudRowsActivity(attempt));
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
    history.length = 0;
    changed.clear();
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
      repositoryId, number: 142, title: "Change 142", author: "maya", headBranch: "feature-142", targetBranch: "main", commits: 1, closed: false,
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

    // A later delivery refreshes the recorded facts.
    expect(await pullRequest("synchronize", "synchronize", 142, { head: { ref: "feature-142b", sha: "d".repeat(40), repo: { id: repositoryId } } }))
      .toBe("pull_request_projected");
    expect((await prEnvironment(142))?.headBranch).toBe("feature-142b");
    // Its api tracks the renamed head Git branch, in Working and Saved.
    const branchOf = (intent: SavedEnvironmentIntent | undefined) => intent?.services.flatMap((node) => node.config.source.type === "git" ? [node.config.source.branch] : [])[0];
    const [working] = await harness.db.select().from(schema.environment).where(eq(schema.environment.id, environmentId));
    const saved = (await harness.db.select().from(schema.environmentSavedStateSnapshot)
      .where(eq(schema.environmentSavedStateSnapshot.environmentId, environmentId))).sort((a, b) => a.createdAt.getTime() - b.createdAt.getTime()).at(-1);
    expect([branchOf(working?.intent), branchOf(saved?.intent as SavedEnvironmentIntent | undefined)]).toEqual([{ type: "connected", name: "feature-142b" }, { type: "connected", name: "feature-142b" }]);

    const asDefault = await harness.runEffect(setProjectDefaultEnvironment({ userId }, {
      organizationSlug: "acme", projectSlug: "shop", environmentId,
    }).pipe(Effect.flip));
    expect(asDefault).toMatchObject({ _tag: "Validation", message: "A PR environment can't be the Default Environment." });
    const asStart = await harness.runEffect(setPrEnvironmentPlan({ userId }, {
      organizationSlug: "acme", projectSlug: "shop", repositoryId, enabled: true, startFromEnvironmentId: environmentId,
      picks: { preset: "only" }, setupCommands: [], removeOnClose: true, includeBots: false,
    }).pipe(Effect.flip));
    expect(asStart).toMatchObject({ _tag: "Validation", message: "Pick an environment that isn't a PR environment." });
  });

  it("names it past a namespace another project of the organization took", async () => {
    await harness.pool.query(`
      insert into project (id, organization_id, name, slug) values (gen_random_uuid(), '${organizationId}', 'Shop PR', 'shop-pr');
      insert into environment (id, project_id, organization_id, name, namespace, intent)
        select gen_random_uuid(), id, '${organizationId}', '142', 'shop-pr-142', '{}' from project where slug = 'shop-pr';
    `);
    expect(await pullRequest("opened", "opened", 142)).toBe("pull_request_projected");
    const [created] = await harness.db.select().from(schema.environment).where(eq(schema.environment.id, (await prEnvironment(142))?.environmentId ?? ""));
    expect(created).toMatchObject({ name: "pr-142-2", namespace: "shop-pr-142-2" });
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
    expect((await prEnvironments()).map((row) => row.number)).toEqual([8]);

    // Turned off: no new ones.
    await setPlan({ enabled: false });
    expect(await pullRequest("off", "opened", 9)).toBe("ignored_pull_request");

    await setPlan({ enabled: true });
    await harness.db.update(schema.environment).set({ intent: { ...stagingIntent, services: stagingIntent.services.filter((node) => node.lineageId === dbLineage) } as never })
      .where(eq(schema.environment.id, stagingId));
    expect(await pullRequest("nothing", "opened", 10)).toBe("ignored_nothing_from_repository");
    expect((await prEnvironments()).map((row) => row.number)).toEqual([8]);
  });

  it("tears it down when it closes, recreates it on reopen or push, and never undoes a newer state", async () => {
    await pullRequest("opened", "opened", 142);
    const first = (await prEnvironment(142))?.environmentId ?? "";

    expect(await pullRequest("closed", "closed", 142)).toBe("pull_request_projected");
    expect(await teardownsOf(first)).toHaveLength(1);
    // Reopened while the old one is still going: a new one starts beside it, under the next free name.
    expect(await pullRequest("reopened", "reopened", 142)).toBe("pull_request_projected");
    const second = (await prEnvironment(142))?.environmentId ?? "";
    expect(second).not.toBe(first);
    const [renamed] = await harness.db.select().from(schema.environment).where(eq(schema.environment.id, second));
    expect(renamed?.name).toBe("pr-142-2");
    await finishTeardown(first);

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
    expect(await harness.db.select().from(schema.prEnvironment)
      .where(and(eq(schema.prEnvironment.projectId, projectId), eq(schema.prEnvironment.number, 143)))).toEqual([]);
  });

  it("follows the pull request's target Git branch, and its Destinations with it, none above it on the same branch", async () => {
    // staging tracks main; dev tracks dev. Each by its latest Saved State.
    const devId = "00000000-0000-4000-8000-000000001105";
    const devIntent = { ...stagingIntent, environmentSlug: "shop-dev", services: stagingIntent.services.map((node) => node.lineageId === apiLineage
      ? { ...node, config: { ...node.config, source: { ...node.config.source, branch: { type: "connected", name: "dev" } } } } : node) };
    await harness.db.insert(schema.environment).values({ id: devId, projectId, organizationId, name: "dev", namespace: "shop-dev", intent: devIntent as never });
    await harness.db.insert(schema.environmentSavedStateSnapshot).values([stagingId, devId].map((environmentId) => ({
      organizationId, environmentId, actorId: userId, intent: (environmentId === devId ? devIntent : stagingIntent) as never, volumeDeletionAuthorizations: [],
    })));
    // Below staging, through a Starting point with no Saved State, hotfix also tracks main: it isn't a Destination.
    const [starterId, hotfixId] = ["00000000-0000-4000-8000-000000001106", "00000000-0000-4000-8000-000000001107"];
    for (const [id, name, parent] of [[starterId, "starter", stagingId], [hotfixId, "hotfix", starterId]] as const) {
      const intent = { ...stagingIntent, environmentSlug: `shop-${name}` };
      await harness.db.insert(schema.environment).values({ id, projectId, organizationId, name, namespace: `shop-${name}`, intent: intent as never });
      await harness.db.insert(schema.environmentBranch).values({
        environmentId: id, organizationId, projectId, parentEnvironmentId: parent, base: stagingIntent as never, createdByUserId: userId,
      });
    }
    await harness.db.insert(schema.environmentSavedStateSnapshot).values({
      organizationId, environmentId: hotfixId, actorId: userId, intent: { ...stagingIntent, environmentSlug: "shop-hotfix" },
      volumeDeletionAuthorizations: [],
    });

    await pullRequest("opened", "opened", 142);
    const environmentId = (await prEnvironment(142))?.environmentId ?? "";
    const destinationsNow = () => harness.runEffect(prDestinations(environmentId));
    expect(await destinationsNow()).toEqual([stagingId]);

    expect(await pullRequest("edited", "edited", 142, { base: { ref: "dev" } })).toBe("pull_request_projected");
    expect((await prEnvironment(142))?.targetBranch).toBe("dev");
    expect(await destinationsNow()).toEqual([devId]);

    // Nothing deploys release: no Destinations.
    await pullRequest("edited-again", "edited", 142, { base: { ref: "release" } });
    expect(await destinationsNow()).toEqual([]);
  });

  describe("saving for a Destination", () => {
    const organizationSlug = "acme";
    const environmentOf = async (id: string) => (await harness.db.select().from(schema.environment).where(eq(schema.environment.id, id)))[0];
    const scope = async (environmentId: string) => {
      const row = await environmentOf(environmentId);
      const serviceId = row?.intent.services.find((node) => node.lineageId === apiLineage)?.id ?? "";
      return { organizationSlug, environmentId, revision: row?.revision ?? "", serviceId };
    };

    /** The review of what goes to staging, computed as the browser does from Org Store rows. */
    async function review(prId: string) {
      const branch = (await prEnvironments()).find((row) => row.environmentId === prId);
      const [pr, staging] = [await environmentOf(prId), await environmentOf(stagingId)];
      if (!branch || !pr || !staging) throw new Error("missing rows");
      return goesTo({
        base: withoutSealedCiphertext(branch.base), kept: false, branch: pr.intent, parent: staging.intent, parentApplied: null,
        hostnames: { branch: branchHostnameSuffix("shop", pr.namespace, true), parent: branchHostnameSuffix("shop", staging.namespace, false) },
      });
    }
    const approve = async (prId: string, picks: (keys: string[], rows: Awaited<ReturnType<typeof review>>["rows"]) => Array<{ key: string; option?: "from" | "new"; value: string }>, shutDown = false) => {
      const { rows, review: string } = await review(prId);
      return runEffect(saveConditionalSave({ userId }, {
        organizationSlug, prEnvironmentId: prId, destinationEnvironmentId: stagingId, shutDown, review: string, picks: picks(rows.map((row) => row.key), rows),
      }));
    };
    const saves = () => harness.db.select().from(schema.conditionalSave);
    const stands = async (prId: string) => {
      const [save] = await saves();
      const [pr, branch] = [await environmentOf(prId), await prEnvironment(142)];
      return save !== undefined && standing(save, pr && { id: pr.id, revision: pr.revision, targetBranch: branch?.targetBranch ?? "" });
    };

    beforeEach(async () => {
      await harness.db.insert(schema.environmentSavedStateSnapshot).values({
        organizationId, environmentId: stagingId, actorId: userId, intent: stagingIntent, volumeDeletionAuthorizations: [],
      });
      await pullRequest("opened", "opened", 142);
    });

    it("keeps the rows on the Destination, sealed, until the PR Environment's settings change", async () => {
      const prId = (await prEnvironment(142))?.environmentId ?? "";
      await runEffect(createServiceVariable({ userId }, { ...(await scope(prId)), key: "FLAG", description: null, exported: false, value: { type: "plain", value: "on" } }));
      await runEffect(createServiceVariable({ userId }, { ...(await scope(prId)), key: "STRIPE_KEY", description: null, exported: false, value: { type: "sealed", value: "sk_test_pr" } }));
      const { rows } = await review(prId);
      const flag = rows.find((row) => variableName(row) === "FLAG")?.key ?? "";
      const stripe = rows.find((row) => variableName(row) === "STRIPE_KEY")?.key ?? "";
      expect(rowLineage({ key: stripe })).toBe(apiLineage);

      // A stale review is refused.
      const stale = await runEffect(saveConditionalSave({ userId }, {
        organizationSlug, prEnvironmentId: prId, destinationEnvironmentId: stagingId, shutDown: false, review: "old", picks: [{ key: flag, value: "" }],
      }).pipe(Effect.as(null), Effect.catch(Effect.succeed)));
      expect(stale).toMatchObject({ _tag: "Conflict", message: "Changed since you reviewed. Review again." });

      // The browser's review string is the server's; a secret takes staging's own value, sealed, never in the browser.
      await approve(prId, () => [{ key: flag, option: "from", value: "" }, { key: stripe, option: "new", value: "sk_live_staging" }]);
      const [save] = await saves();
      expect(save).toMatchObject({
        prEnvironmentId: prId, prNumber: 142, repositoryId, destinationEnvironmentId: stagingId, targetBranch: "main", savedByUserId: userId,
        workingRevision: (await environmentOf(prId))?.revision,
      });
      expect(save?.rows.map((row) => [row.row.key, row.option])).toEqual([[flag, "from"], [stripe, "new"]]);
      expect(save?.landing.identities.services.map((row) => row.lineageId)).toEqual([apiLineage]);
      expect(await stands(prId)).toBe(true);
      expect(JSON.stringify(save?.picks)).not.toContain("sk_live_staging");
      expect(save?.picks.find((pick) => pick.key === stripe)?.choice).toMatchObject({ option: "new", value: { valueFingerprint: expect.any(String) } });
      const read = await harness.runEffect(readCollection({ userId }, { table: "conditional_save", userId, organizationSlug }));
      expect(read.rows).toEqual([expect.objectContaining({ id: save?.id })]);
      expect(JSON.stringify(read.rows)).not.toMatch(/encryptedValue|ingerprint|landing|picks/u);

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
      // Torn down while the pull request stays open: retired, it loses its approvals, and a new one starts unapproved.
      await runEffect(closePrEnvironment(prId));
      await pullRequest("synchronize", "synchronize", 142);
      expect(await saves()).toEqual([]);
      expect((await prEnvironment(142))?.environmentId).not.toBe(prId);
    });

    it("shuts down keeping its rows and its save, stays Off through Undo, and starts again the same on the next push or Deploy", async () => {
      const prId = (await prEnvironment(142))?.environmentId ?? "";
      await runEffect(createServiceVariable({ userId }, { ...(await scope(prId)), key: "FLAG", description: null, exported: false, value: { type: "plain", value: "on" } }));
      const tickAll = (keys: string[]) => keys.map((key) => ({ key, option: "from" as const, value: "" }));
      const shutdown = async () => (await prEnvironment(142))?.shutdown ?? null;
      // Saving with "Shut down pr-142 now" shuts it down in the same call. A shutdown that fails leaves the save standing.
      runtimeFails = true;
      expect(await approve(prId, tickAll, true)).toMatchObject({ shutDown: false });
      expect(await stands(prId)).toBe(true);
      expect(await shutdown()).toBe(null);
      runtimeFails = false;
      expect(await approve(prId, tickAll, true)).toMatchObject({ shutDown: true });
      expect(await stands(prId)).toBe(true);
      await harness.db.update(schema.service).set({ firstDeployedAt: new Date() }).where(eq(schema.service.environmentId, prId));
      const before = await environmentOf(prId);
      /** The teardown workflow ends a shutdown: its runtime half done, or failed. */
      const finish = async (attempt: typeof schema.teardownAttempt.$inferSelect, status: "completed" | "failed" = "completed") => {
        const inngestRunId = `run-${attempt.id}`;
        await harness.db.update(schema.teardownAttempt).set({ status: "running", inngestRunId, startedAt: new Date() })
          .where(eq(schema.teardownAttempt.id, attempt.id));
        if (status === "completed") await runEffect(finishShutdownActivity(attempt));
        await runEffect(status === "completed"
          ? completeTeardownAttempt({ attemptId: attempt.id, inngestRunId, status, outcome: { pairingRevocationUnconfirmed: false, runtimeMembership: "untouched" } })
          : completeTeardownAttempt({ attemptId: attempt.id, inngestRunId, status, failureMessage: "The runtime refused." }));
      };
      const shutDown = async () => {
        await runEffect(shutDownPrEnvironment({ userId }, { organizationSlug, environmentId: prId }));
        const attempt = (await teardownsOf(prId)).find((row) => row.status === "pending");
        if (!attempt) throw new Error("No shutdown was admitted.");
        return attempt;
      };

      // Its runtime half only: its namespace's data confirmed, its attempt cancelled, every row kept.
      const attempt = (await teardownsOf(prId)).find((row) => row.status === "pending");
      if (!attempt) throw new Error("No shutdown was admitted.");
      expect(attempt).toMatchObject({ scope: "shutdown", environmentId: prId, confirmDataLoss: [expect.objectContaining({ id: expect.objectContaining({ name: "shop-pr-142-data" }) })] });
      expect(await shutdown()).toBe("running");
      expect((await deploymentsOf(prId)).map((row) => row.status)).not.toContain("queued");
      // Shut down again while it runs does nothing.
      await runEffect(shutDownPrEnvironment({ userId }, { organizationSlug, environmentId: prId }));
      expect((await teardownsOf(prId)).length).toBe(1);
      // Deploying waits for it to finish; once it has, its services' Setup Commands run again.
      expect(await runEffect(startPrEnvironment({ userId }, { organizationSlug, environmentId: prId }).pipe(Effect.as(null), Effect.catch(Effect.succeed))))
        .toMatchObject({ _tag: "Conflict", message: "This environment is shutting down. Deploy it once it's off." });
      await finish(attempt);
      expect(await shutdown()).toBe("off");
      expect(await environmentOf(prId)).toMatchObject({ revision: before?.revision, intent: before?.intent });
      expect((await harness.db.select().from(schema.service).where(eq(schema.service.environmentId, prId))).map((row) => row.firstDeployedAt)).toEqual([null]);
      expect(await stands(prId)).toBe(true);

      // Undo keeps it Off.
      await runEffect(withdrawConditionalSave({ userId }, { organizationSlug, prEnvironmentId: prId, destinationEnvironmentId: stagingId }));
      expect(await shutdown()).toBe("off");
      await approve(prId, tickAll);

      // The next push deploys the same Environment again, and the save still stands.
      heads.set("feature-142", "d".repeat(40));
      await deliver("push", "push-off", { ref: "refs/heads/feature-142", before: "c".repeat(40), after: "d".repeat(40), created: false, deleted: false, forced: false });
      expect(await shutdown()).toBe(null);
      expect((await prEnvironment(142))?.environmentId).toBe(prId);
      expect((await deploymentsOf(prId)).map((row) => row.triggerOrigin.origin)).toContain("github");
      expect(await stands(prId)).toBe(true);

      // A failed shutdown isn't Off: Shut down runs again. Or Deploy starts it again.
      await finish(await shutDown(), "failed");
      expect(await shutdown()).toBe("failed");
      await finish(await shutDown());
      expect(await shutdown()).toBe("off");
      const { deploymentId } = await runEffect(startPrEnvironment({ userId }, { organizationSlug, environmentId: prId }));
      expect(await shutdown()).toBe(null);
      expect((await deploymentsOf(prId)).find((row) => row.id === deploymentId)?.triggerOrigin.origin).toBe("manual");

      // Closing removes an Off PR Environment as it does a running one.
      await shutDown();
      await pullRequest("closed", "closed", 142);
      expect((await teardownsOf(prId)).map((row) => [row.scope, row.status])).toContainEqual(["environment", "pending"]);
    });

    it("refuses an empty new value, and picks core couldn't land", async () => {
      const prId = (await prEnvironment(142))?.environmentId ?? "";
      await runEffect(createServiceVariable({ userId }, { ...(await scope(prId)), key: "FLAG", description: null, exported: false, value: { type: "plain", value: "on" } }));
      const worker = await runEffect(createService({ userId }, {
        organizationSlug, environmentId: prId, name: "Worker", source: createImageServiceSource({ image: "acme/worker:1" }), x: 5, y: 6,
        preDeployCommand: null, startCommand: null, healthcheck: { type: "none" }, restartPolicy: "unless-stopped",
      }));
      const workerScope = { ...(await scope(prId)), serviceId: worker.data.service.id, revision: (await environmentOf(prId))?.revision ?? "" };
      await runEffect(createServiceVariable({ userId }, { ...workerScope, key: "QUEUE", description: null, exported: false, value: { type: "plain", value: "jobs" } }));
      const { rows, review: string } = await review(prId);
      const flag = rows.find((row) => variableName(row) === "FLAG")?.key ?? "";
      const queue = rows.find((row) => variableName(row) === "QUEUE")?.key ?? "";
      const approveOnly = (picks: Array<{ key: string; option?: "from" | "new"; value: string }>) => runEffect(saveConditionalSave({ userId }, {
        organizationSlug, prEnvironmentId: prId, destinationEnvironmentId: stagingId, shutDown: false, review: string, picks,
      }).pipe(Effect.as(null), Effect.catch(Effect.succeed)));

      // The new service's variable without the service: core refuses it.
      expect(await approveOnly([{ key: queue, option: "from", value: "" }])).toMatchObject({ _tag: "Validation" });
      // Every new value comes with the save.
      expect(await approveOnly([{ key: flag, option: "new", value: "" }])).toMatchObject({ _tag: "Validation", message: "Enter a new value for FLAG." });
      expect(await saves()).toEqual([]);
    });

    it("takes no save once its pull request closes, even with its environment kept", async () => {
      await setPlan({ removeOnClose: false });
      const prId = (await prEnvironment(142))?.environmentId ?? "";
      await runEffect(createServiceVariable({ userId }, { ...(await scope(prId)), key: "FLAG", description: null, exported: false, value: { type: "plain", value: "on" } }));
      await pullRequest("closed", "closed", 142);
      const { rows, review: string } = await review(prId);
      const refused = await runEffect(saveConditionalSave({ userId }, {
        organizationSlug, prEnvironmentId: prId, destinationEnvironmentId: stagingId, shutDown: false, review: string, picks: rows.map((row) => ({ key: row.key, option: "from" as const, value: "" })),
      }).pipe(Effect.as(null), Effect.catch(Effect.succeed)));
      expect(refused).toMatchObject({ _tag: "Conflict", message: "PR #142 is closed." });
      expect(await saves()).toEqual([]);
    });

    it("posts the Ployz · ready to merge check on the head commit, and updates it as the PR Environment changes", async () => {
      const prId = (await prEnvironment(142))?.environmentId ?? "";
      const [first, second] = ["c".repeat(40), "d".repeat(40)];
      const runOn = (sha: string) => {
        const runs = checkRuns.filter((run) => run.headSha === sha && run.body.external_id === `${repositoryId}:142`);
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

      // Opened: nothing here staging lacks yet. Its first Working and Saved writes each ask for the check.
      expect(await postChecks()).toEqual(["posted", "posted"]);
      expect(runOn(first)).toEqual({
        conclusion: "success", reason: "No changes for staging",
        detailsUrl: expect.stringMatching(/\/cloud\/acme\/shop\/shop-pr-142$/u),
        summary: expect.stringContaining("- https://api-pr-142.acme.ployz.test"),
      });

      // A setting edited on the PR Environment updates the same check run.
      await runEffect(createServiceVariable({ userId }, { ...(await scope(prId)), key: "STRIPE_KEY", description: null, exported: false, value: { type: "sealed", value: "sk_test_pr" } }));
      await postChecks();
      expect(runOn(first)).toMatchObject({ conclusion: "action_required", reason: "1 change to save in Ployz" });

      // A new head gets its own.
      await pullRequest("synchronize", "synchronize", 142, { head: { ref: "feature-142", sha: second, repo: { id: repositoryId } } });
      expect(await postChecks()).toEqual(["posted"]);
      expect(runOn(second)).toMatchObject({ conclusion: "action_required", reason: "1 change to save in Ployz" });

      // Saved with staging's value.
      await approve(prId, (keys) => keys.map((key) => ({ key, option: "new" as const, value: "sk_live" })));
      await postChecks();
      expect(runOn(second)).toMatchObject({ conclusion: "success", reason: "1 change goes live with this PR" });

      // Undo.
      await runEffect(withdrawConditionalSave({ userId }, { organizationSlug, prEnvironmentId: prId, destinationEnvironmentId: stagingId }));
      await postChecks();
      expect(runOn(second)).toMatchObject({ conclusion: "action_required", reason: "1 change to save in Ployz" });

      // Another pull request from the same head posts its own run after ours: ours is still found, not duplicated.
      const ours = checkRuns.find((run) => run.headSha === second);
      if (ours) checkRuns.push({ id: checkRuns.length + 1, headSha: second, body: { ...ours.body, external_id: `${repositoryId}:999` } });
      expect(await runEffect(postPrCheck({ repositoryId, number: 142 }))).toBe("posted");
      expect(runOn(second)).toMatchObject({ reason: "1 change to save in Ployz" });

      // Without Checks: write nothing is posted, and nothing fails.
      checksForbidden = true;
      await approve(prId, (keys) => keys.map((key) => ({ key, option: "new" as const, value: "sk_live" })));
      expect(await postChecks()).toEqual(["forbidden"]);
      expect(runOn(second)).toMatchObject({ reason: "1 change to save in Ployz" });
      expect(checkRuns).toHaveLength(3);

      // A pull request without a PR Environment gets none.
      checksForbidden = false;
      await setPlan({ enabled: false });
      expect(await pullRequest("other", "opened", 9, { head: { ref: "other", sha: "e".repeat(40), repo: { id: repositoryId } } })).toBe("ignored_pull_request");
      expect(await postChecks()).toEqual([]);
      expect(checkRuns).toHaveLength(3);
    });

    it("re-posts the check as passing when staging takes the PR Environment's change", async () => {
      const prId = (await prEnvironment(142))?.environmentId ?? "";
      const title = () => checkRuns.find((run) => run.body.external_id === `${repositoryId}:142`)?.body;
      const stripe = { key: "STRIPE_KEY", description: null, exported: false, value: { type: "plain" as const, value: "sk_test" } };
      await runEffect(createServiceVariable({ userId }, { ...(await scope(prId)), ...stripe }));
      await postChecks();
      expect(title()).toMatchObject({ conclusion: "action_required" });

      await runEffect(createServiceVariable({ userId }, { ...(await scope(stagingId)), ...stripe }));
      expect(await postChecks()).toEqual(["posted"]);
      expect(title()).toMatchObject({ conclusion: "success", output: { title: "No changes for staging" } });
    });

    describe("when the pull request closes", () => {
      const merged = { state: "closed" as const, merged: true, merge_commit_sha: "e".repeat(40) };
      type Intent = { services: Array<{ id: string; lineageId: string; variables: Array<{ key: string; value: unknown }> }> };
      const variableIn = (intent: Intent | undefined, key: string) =>
        intent?.services.find((node) => node.lineageId === apiLineage)?.variables.find((variable) => variable.key === key)?.value;
      const plain = (value: string) => ({ kind: "literal", value });
      const setVariable = async (environmentId: string, key: string, value: string) => {
        const scoped = await scope(environmentId);
        const existing = (await environmentOf(environmentId))?.intent.services.find((node) => node.lineageId === apiLineage)?.variables.find((variable) => variable.key === key);
        const input = { ...scoped, key, description: null, exported: false, value: { type: "plain" as const, value } };
        await runEffect(existing ? updateServiceVariable({ userId }, { ...input, variableId: existing.id }) : createServiceVariable({ userId }, input));
      };
      /** staging's Working State becomes its latest Saved revision. */
      const saveStaging = async () => harness.db.insert(schema.environmentSavedStateSnapshot).values({
        organizationId, environmentId: stagingId, actorId: userId, intent: (await environmentOf(stagingId))?.intent, volumeDeletionAuthorizations: [],
      });
      const latestSaved = async () => (await harness.db.select().from(schema.environmentSavedStateSnapshot)
        .where(eq(schema.environmentSavedStateSnapshot.environmentId, stagingId))).sort((a, b) => a.createdAt.getTime() - b.createdAt.getTime()).at(-1);
      const tickAll = (keys: string[]) => keys.map((key) => ({ key, option: "from" as const, value: "" }));

      it("drops an approval for an Environment that stopped being a Destination before the merge", async () => {
        const prId = (await prEnvironment(142))?.environmentId ?? "";
        await setVariable(prId, "FLAG", "on");
        await approve(prId, tickAll);
        // staging's api now deploys another Git branch.
        const intent = (await environmentOf(stagingId))?.intent;
        const api = intent?.services.find((node) => node.id === apiId);
        if (api?.config.source.type === "git") api.config.source.branch = { type: "connected", name: "release" };
        await harness.db.update(schema.environment).set({ intent }).where(eq(schema.environment.id, stagingId));
        await saveStaging();
        const before = (await latestSaved())?.id;

        await pullRequest("merged", "closed", 142, merged);
        expect(await saves()).toEqual([]);
        expect((await latestSaved())?.id).toBe(before);
      });

      it("lands where nothing deploys on push: each setting as production left it, changed it live, or has an edit of its own", async () => {
        await setVariable(stagingId, "MODE", "a");
        await setVariable(stagingId, "LEVEL", "1");
        await setVariable(stagingId, "TIER", "free");
        await saveStaging();
        await pullRequest("opened-150", "opened", 150);
        const prId = (await prEnvironment(150))?.environmentId ?? "";
        await setVariable(prId, "MODE", "b");
        await setVariable(prId, "LEVEL", "2");
        await setVariable(prId, "TIER", "pro");
        await setVariable(prId, "FLAG", "on");
        await runEffect(createServiceVariable({ userId }, { ...(await scope(prId)), key: "STRIPE_KEY", description: null, exported: false, value: { type: "sealed", value: "sk_test" } }));
        // A service the pull request adds.
        const worker = await runEffect(createService({ userId }, {
          organizationSlug, environmentId: prId, name: "Worker", source: createImageServiceSource({ image: "acme/worker:1" }), x: 5, y: 6,
          preDeployCommand: null, startCommand: null, healthcheck: { type: "none" }, restartPolicy: "unless-stopped",
        }));
        const workerLineage = worker.data.service.lineageId;
        // TIER goes with a value of staging's own.
        await approve(prId, (_, rows) => rows.map((row) => variableName(row) === "STRIPE_KEY" ? { key: row.key, option: "new" as const, value: "sk_live" }
          : variableName(row) === "TIER" ? { key: row.key, option: "new" as const, value: "business" }
          : row.role === "move" && row.choice ? { key: row.key, option: "from" as const, value: "" } : { key: row.key, value: "" }));
        const [saved150] = await saves();
        expect(saved150?.rows.some(({ row }) => row.key === `${workerLineage}:node`)).toBe(true);

        // Meanwhile staging changes MODE live; stages its own LEVEL; and changes TIER live, then stages another TIER.
        await setVariable(stagingId, "MODE", "c");
        await setVariable(stagingId, "TIER", "team");
        await saveStaging();
        await setVariable(stagingId, "LEVEL", "5");
        await setVariable(stagingId, "TIER", "enterprise");
        const before = (await latestSaved())?.id;

        expect(await pullRequest("merged-150", "closed", 150, merged)).toBe("pull_request_projected");
        const saved = await latestSaved();
        expect(saved?.id).not.toBe(before);
        const savedIntent = saved?.intent as Intent | undefined;
        const working = (await environmentOf(stagingId))?.intent;
        const values = (intent: Intent | undefined) => ["FLAG", "LEVEL", "MODE", "TIER"].map((key) => variableIn(intent, key));
        // Untouched: saved. Its own edit: saved, the edit on top. Changed live: a change to deploy. Both: its edit stays.
        expect(values(savedIntent)).toEqual([plain("on"), plain("2"), plain("c"), plain("team")]);
        expect(values(working)).toEqual([plain("on"), plain("5"), plain("b"), plain("enterprise")]);
        expect(variableIn(working, "STRIPE_KEY")).toMatchObject({ kind: "secret" });

        // The added service has identities in staging, the same in both states.
        const arrived = working?.services.find((node) => node.lineageId === workerLineage);
        expect(arrived?.id).toBeDefined();
        expect(savedIntent?.services.find((node) => node.lineageId === workerLineage)?.id).toBe(arrived?.id);
        const [identity] = await harness.db.select().from(schema.service).where(eq(schema.service.id, arrived?.id ?? ""));
        expect(identity).toMatchObject({ environmentId: stagingId, lineageId: workerLineage, name: "Worker" });

        // Left: MODE tagged "PR #150", TIER a hint. The PR Environment's teardown keeps it.
        const [marker, ...none] = await saves();
        expect(none).toEqual([]);
        expect(marker).toMatchObject({ prEnvironmentId: null, mergeCommitSha: "e".repeat(40), landedSavedStateId: saved?.id });
        expect(marker?.rows.map(({ row, landed }) => [variableName(row), landed])).toEqual([["MODE", "staged"], ["TIER", "hint"]]);
        await finishTeardown(prId);
        expect(await saves()).toHaveLength(1);

        // Use: PR #150's TIER, the value it saved, replaces staging's edit, as a change to deploy tagged like MODE.
        const tierRow = marker?.rows.find(({ row }) => variableName(row) === "TIER")?.row;
        expect(tierRow && presentRow(tierRow, () => "api").after).toBe("business");
        const tier = tierRow?.key ?? "";
        const take = (key: string) => runEffect(takePullRequestValue({ userId }, { organizationSlug, conditionalSaveId: marker?.id ?? "", key })
          .pipe(Effect.as(null), Effect.catch(Effect.succeed)));
        expect(await take(tier)).toBeNull();
        expect(variableIn((await environmentOf(stagingId))?.intent, "TIER")).toEqual(plain("business"));
        expect((await saves())[0]?.rows.map(({ landed }) => landed)).toEqual(["staged", "staged"]);
        expect(await take(tier)).toMatchObject({ _tag: "Conflict" });
      });

      it("lands nothing when withdrawn or unmerged, waits where staging deploys on push, and Merge refuses a PR Environment", async () => {
        const prId = (await prEnvironment(142))?.environmentId ?? "";
        await setVariable(prId, "FLAG", "on");
        const snapshots = async () => (await harness.db.select().from(schema.environmentSavedStateSnapshot)
          .where(eq(schema.environmentSavedStateSnapshot.environmentId, stagingId))).length;
        const count = await snapshots();

        const refused = await harness.runEffect(saveBranch({ userId }, {
          organizationSlug, branchEnvironmentId: prId, destinationRevision: (await environmentOf(stagingId))?.revision ?? "", review: "", picks: [], thenDelete: false,
        }).pipe(Effect.flip, Effect.provideService(OrganizationRuntime, runtime), Effect.provideService(InngestClient, inngest)));
        expect(refused).toMatchObject({ _tag: "Conflict", message: "A PR environment's changes go live with PR #142." });

        // Withdrawn by an edit, then merged: nothing lands.
        await setPlan({ removeOnClose: false });
        await approve(prId, tickAll);
        await setVariable(prId, "FLAG", "off");
        await pullRequest("merged", "closed", 142, merged);
        expect(await saves()).toEqual([]);
        expect(await snapshots()).toBe(count);

        // Closed without merging: dropped.
        await pullRequest("reopened", "reopened", 142);
        await approve(prId, tickAll);
        await pullRequest("closed", "closed", 142);
        expect(await saves()).toEqual([]);

        // staging deploys main on push: frozen, waiting for the merge commit's deployment.
        await harness.db.update(schema.service).set({ policy: { ...policy, autoDeploy: true, imageUpdate: { type: "off" as const } } }).where(eq(schema.service.id, apiId));
        await pullRequest("reopened-again", "reopened", 142);
        await approve(prId, tickAll);
        await pullRequest("merged-again", "closed", 142, merged);
        expect(await saves()).toEqual([expect.objectContaining({ prEnvironmentId: null, mergeCommitSha: "e".repeat(40), landedSavedStateId: null })]);
        expect(await snapshots()).toBe(count);
      });

      describe("where staging deploys main on push", () => {
        const mergeSha = "e".repeat(40);
        /** A commit lands on main touching `paths`, and its push arrives. */
        async function push(sha: string, paths: string[], onMain = true) {
          const before = heads.get("main") ?? "0".repeat(40);
          heads.set("main", sha);
          if (onMain) history.push(sha);
          changed.set(sha, paths);
          await deliver("push", `push-${sha.slice(0, 1)}`, { ref: "refs/heads/main", before, after: sha, created: false, deleted: false, forced: false });
        }
        /** CI passes on `sha`, and the sweep resumes waiting triggers. */
        async function ciPasses(sha: string) {
          const [delivery] = await harness.db.select().from(schema.githubWebhookDelivery);
          await harness.db.insert(schema.githubCheckSuiteProjection).values({
            installationId, repositoryId, checkSuiteId: history.indexOf(sha) + 1, headSha: sha, status: "completed", conclusion: "success",
            sourceUpdatedAt: new Date(), lastDeliveryId: delivery?.deliveryId ?? "", lastReceiptSequence: delivery?.receiptSequence ?? 0,
          });
          await runEffect(resumeGithubWaitingTriggers());
        }
        const waitForCi = () => harness.db.update(schema.service).set({ policy: { ...policy, autoDeploy: true, waitForCi: true, imageUpdate: { type: "off" as const } } })
          .where(eq(schema.service.id, apiId));
        /** The staging attempt building `sha`, and its Saved revision. */
        async function attemptAt(sha: string) {
          const attempt = (await deploymentsOf(stagingId)).find((row) => row.sourcePins[apiId]?.commitSha === sha);
          const [saved] = attempt ? await harness.db.select().from(schema.environmentSavedStateSnapshot)
            .where(eq(schema.environmentSavedStateSnapshot.id, attempt.savedStateSnapshotId)) : [];
          return { attempt, saved: saved?.intent as Intent | undefined };
        }
        const triggers = () => harness.db.select().from(schema.githubEnvironmentTrigger).where(eq(schema.githubEnvironmentTrigger.environmentId, stagingId));

        beforeEach(async () => {
          await harness.db.update(schema.service).set({ policy: { ...policy, autoDeploy: true, imageUpdate: { type: "off" as const } } }).where(eq(schema.service.id, apiId));
          await push("a".repeat(40), []);
          await setVariable((await prEnvironment(142))?.environmentId ?? "", "FLAG", "on");
        });

        it("ships the approved rows and the services they add in the merge commit's deployment, and nothing in one without it", async () => {
          const prId = (await prEnvironment(142))?.environmentId ?? "";
          const worker = await runEffect(createService({ userId }, {
            organizationSlug, environmentId: prId, name: "Worker", source: createImageServiceSource({ image: "acme/worker:1" }), x: 5, y: 6,
            preDeployCommand: null, startCommand: null, healthcheck: { type: "none" }, restartPolicy: "unless-stopped",
          }));
          await approve(prId, (_, rows) => rows.map((row) => row.role === "move" && row.choice ? { key: row.key, option: "from" as const, value: "" } : { key: row.key, value: "" }));
          await pullRequest("merged", "closed", 142, merged);
          const before = (await latestSaved())?.id;

          // A commit on main without the merge commit carries nothing.
          await push("b".repeat(40), ["api/main.ts"], false);
          expect((await attemptAt("b".repeat(40))).saved?.services.map((node) => node.lineageId)).toEqual([apiLineage, dbLineage]);
          expect((await latestSaved())?.id).toBe(before);
          expect(await saves()).toHaveLength(1);

          history.push("b".repeat(40));
          await push(mergeSha, ["api/main.ts"]);
          const { attempt, saved } = await attemptAt(mergeSha);
          expect(attempt?.savedStateSnapshotId).toBe((await latestSaved())?.id);
          expect(variableIn(saved, "FLAG")).toEqual(plain("on"));
          expect(saved?.services.some((node) => node.lineageId === worker.data.service.lineageId)).toBe(true);
          expect(await saves()).toEqual([]);
        });

        it("admits the merge commit's push without a Git service the approval points at another repository", async () => {
          const worker = await runEffect(createService({ userId }, {
            organizationSlug, environmentId: stagingId, name: "Worker", x: 5, y: 6,
            source: createGitServiceSource({ repository: "acme/app", repositoryId, access: { type: "github-installation", installationId } }),
            preDeployCommand: null, startCommand: null, healthcheck: { type: "none" }, restartPolicy: "unless-stopped",
          }));
          const workerId = worker.data.service.id;
          await harness.db.update(schema.service).set({ policy: { ...policy, autoDeploy: true, imageUpdate: { type: "off" as const } } }).where(eq(schema.service.id, workerId));
          await harness.db.insert(schema.environmentSavedStateSnapshot).values({
            organizationId, environmentId: stagingId, actorId: userId, intent: (await environmentOf(stagingId))?.intent ?? stagingIntent, volumeDeletionAuthorizations: [],
          });
          await pullRequest("opened-150", "opened", 150);
          const prId = (await prEnvironment(150))?.environmentId ?? "";
          // Its Worker is pointed at another repository.
          const intent = (await environmentOf(prId))?.intent;
          const prWorker = intent?.services.find((node) => node.lineageId === worker.data.service.lineageId);
          if (prWorker) prWorker.config.source = createGitServiceSource({ repository: "acme/other", repositoryId: 43, access: { type: "github-installation", installationId } });
          await harness.db.update(schema.environment).set({ intent }).where(eq(schema.environment.id, prId));
          await approve(prId, (_, rows) => rows.map((row) => row.role === "move" && row.choice ? { key: row.key, option: "from" as const, value: "" } : { key: row.key, value: "" }));
          await pullRequest("merged-150", "closed", 150, merged);

          await push(mergeSha, ["api/main.ts"]);
          const { attempt, saved } = await attemptAt(mergeSha);
          expect(Object.keys(attempt?.sourcePins ?? {})).toEqual([apiId]);
          expect((saved?.services.find((node) => node.id === workerId) as { config?: { source?: unknown } } | undefined)?.config?.source).toMatchObject({ repository: "acme/other" });
          expect(await saves()).toEqual([]);
        });

        it("ships two pull requests' approvals in one deployment, with a new secret the same in both states", async () => {
          await approve((await prEnvironment(142))?.environmentId ?? "", tickAll);
          await pullRequest("opened-150", "opened", 150);
          const second = (await prEnvironment(150))?.environmentId ?? "";
          await runEffect(createServiceVariable({ userId }, { ...(await scope(second)), key: "STRIPE_KEY", description: null, exported: false, value: { type: "sealed", value: "sk_test" } }));
          await approve(second, (keys) => keys.map((key) => ({ key, option: "new" as const, value: "sk_live" })));
          const secondMerge = "f".repeat(40);
          await pullRequest("merged", "closed", 142, merged);
          await pullRequest("merged-150", "closed", 150, { ...merged, merge_commit_sha: secondMerge });
          history.push(mergeSha);
          await push(secondMerge, ["api/main.ts"]);

          const { saved } = await attemptAt(secondMerge);
          expect(variableIn(saved, "FLAG")).toEqual(plain("on"));
          expect(variableIn(saved, "STRIPE_KEY")).toMatchObject({ kind: "secret" });
          expect(await saves()).toEqual([]);
          const idOf = (intent: Intent | undefined) => intent?.services.find((node) => node.lineageId === apiLineage)?.variables
            .find((variable) => variable.key === "STRIPE_KEY") as { id?: string } | undefined;
          expect(idOf((await environmentOf(stagingId))?.intent as Intent | undefined)?.id).toBe(idOf(saved)?.id);
        });

        it("lands two pull requests' approvals in the one push carrying both merges, before either closed delivery", async () => {
          await approve((await prEnvironment(142))?.environmentId ?? "", tickAll);
          await pullRequest("opened-150", "opened", 150);
          const second = (await prEnvironment(150))?.environmentId ?? "";
          await setVariable(second, "MODE", "fast");
          await approve(second, tickAll);
          const secondMerge = "f".repeat(40);
          mergedOnGithub(142, merged);
          mergedOnGithub(150, { ...merged, merge_commit_sha: secondMerge });
          history.push(mergeSha);
          await push(secondMerge, ["api/main.ts"]);

          const { saved } = await attemptAt(secondMerge);
          expect(variableIn(saved, "FLAG")).toEqual(plain("on"));
          expect(variableIn(saved, "MODE")).toEqual(plain("fast"));
          expect(await saves()).toEqual([]);
        });

        it("lands at the merge commit's push when it comes before the closed delivery, by asking GitHub, and takes no approval after", async () => {
          await approve((await prEnvironment(142))?.environmentId ?? "", tickAll);
          mergedOnGithub(142, merged);
          await push(mergeSha, ["api/main.ts"]);
          expect(variableIn((await attemptAt(mergeSha)).saved, "FLAG")).toEqual(plain("on"));
          expect(await saves()).toEqual([]);

          // Merged, as the push showed: no approval after it.
          const prId = (await prEnvironment(142))?.environmentId ?? "";
          const { rows, review: string } = await review(prId);
          const refused = await runEffect(saveConditionalSave({ userId }, {
            organizationSlug, prEnvironmentId: prId, destinationEnvironmentId: stagingId, shutDown: false, review: string, picks: rows.map((row) => ({ key: row.key, option: "from" as const, value: "" })),
          }).pipe(Effect.as(null), Effect.catch(Effect.succeed)));
          expect(refused).toMatchObject({ _tag: "Conflict", message: "PR #142 is closed." });

          const count = (await harness.db.select().from(schema.environmentSavedStateSnapshot)).length;
          await pullRequest("merged", "closed", 142, merged);
          expect((await harness.db.select().from(schema.environmentSavedStateSnapshot)).length).toBe(count);
        });

        it("waits with a trigger waiting for CI, and lands with a descendant when the merge commit's trigger is superseded", async () => {
          await waitForCi();
          await approve((await prEnvironment(142))?.environmentId ?? "", tickAll);
          await pullRequest("merged", "closed", 142, merged);
          const before = (await latestSaved())?.id;
          await push(mergeSha, ["api/main.ts"]);
          await push("f".repeat(40), ["api/next.ts"]);
          expect((await latestSaved())?.id).toBe(before);
          expect(await saves()).toHaveLength(1);

          await ciPasses("f".repeat(40));
          expect((await triggers()).filter((row) => row.headSha === mergeSha).map((row) => row.admissionState)).toEqual(["superseded"]);
          expect(variableIn((await attemptAt("f".repeat(40))).saved, "FLAG")).toEqual(plain("on"));
          expect(await saves()).toEqual([]);
        });

        it("saves past an older trigger still waiting for CI once it's superseded, when the processed head has the merge commit", async () => {
          await waitForCi();
          await approve((await prEnvironment(142))?.environmentId ?? "", tickAll);
          await push("b".repeat(40), ["api/main.ts"]);
          await push(mergeSha, ["docs/readme.md"]);
          expect((await triggers()).find((row) => row.headSha === "b".repeat(40))?.admissionState).toBe("waiting");
          await pullRequest("merged", "closed", 142, merged);
          await runEffect(resumeGithubWaitingTriggers());
          expect(variableIn((await latestSaved())?.intent as Intent | undefined, "FLAG")).toEqual(plain("on"));
          expect(await saves()).toEqual([]);
        });

        it("freezes nothing of a PR Environment being torn down at a merge push, and drops its approvals when retired", async () => {
          const prId = (await prEnvironment(142))?.environmentId ?? "";
          await approve(prId, tickAll);
          await runEffect(closePrEnvironment(prId));
          mergedOnGithub(142, merged);
          await push(mergeSha, ["api/main.ts"]);
          expect(variableIn((await attemptAt(mergeSha)).saved, "FLAG")).toBeUndefined();
          expect((await saves()).map((row) => row.state)).toEqual(["standing"]);
          await pullRequest("synchronize", "synchronize", 142);
          expect(await saves()).toEqual([]);
        });

        it("lands what a superseded trigger carried: an unlinked merge push waiting for CI, then an unwatched descendant, close, the sweep", async () => {
          await waitForCi();
          await approve((await prEnvironment(142))?.environmentId ?? "", tickAll);
          await push(mergeSha, ["api/main.ts"]);
          await push("f".repeat(40), ["docs/readme.md"]);
          await pullRequest("merged", "closed", 142, merged);
          expect((await triggers()).find((row) => row.headSha === mergeSha)?.conditionalSaveIds).toHaveLength(1);
          await runEffect(resumeGithubWaitingTriggers());
          expect((await triggers()).find((row) => row.headSha === mergeSha)?.admissionState).toBe("superseded");
          expect(variableIn((await latestSaved())?.intent as Intent | undefined, "FLAG")).toEqual(plain("on"));
          expect(await saves()).toEqual([]);
        });

        it("lands through a trigger already waiting for CI when the closed delivery arrives", async () => {
          await waitForCi();
          await approve((await prEnvironment(142))?.environmentId ?? "", tickAll);
          // GitHub doesn't yet know the commit merged the pull request.
          await push(mergeSha, ["api/main.ts"]);
          expect((await triggers()).find((row) => row.headSha === mergeSha)?.conditionalSaveIds).toEqual([]);
          await pullRequest("merged", "closed", 142, merged);
          const [save] = await saves();
          expect((await triggers()).find((row) => row.headSha === mergeSha)?.conditionalSaveIds).toEqual([save?.id]);

          await ciPasses(mergeSha);
          expect(variableIn((await attemptAt(mergeSha)).saved, "FLAG")).toEqual(plain("on"));
          expect(await saves()).toEqual([]);
        });

        it("saves the held changes at a merge commit's push that deploys nothing", async () => {
          await approve((await prEnvironment(142))?.environmentId ?? "", tickAll);
          await pullRequest("merged", "closed", 142, merged);
          await push(mergeSha, ["docs/readme.md"]);
          expect((await attemptAt(mergeSha)).attempt).toBeUndefined();
          expect(variableIn((await latestSaved())?.intent as Intent | undefined, "FLAG")).toEqual(plain("on"));
          expect(await saves()).toEqual([]);
        });

        it("saves at close when a merge commit's push GitHub hadn't linked yet deployed nothing", async () => {
          await approve((await prEnvironment(142))?.environmentId ?? "", tickAll);
          await push(mergeSha, ["docs/readme.md"]);
          expect(await saves()).toHaveLength(1);
          await pullRequest("merged", "closed", 142, merged);
          expect(variableIn((await latestSaved())?.intent as Intent | undefined, "FLAG")).toEqual(plain("on"));
          expect(await saves()).toEqual([]);
        });
      });
    });
  });
});
