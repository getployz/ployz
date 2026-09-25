import crypto from "node:crypto";
import { gzipSync } from "node:zlib";
import { InngestTestEngine, mockCtx } from "@inngest/test";
import type { BuildOptions, BuildOutcome, BuildReceipts, Client, Machine, MachineDetails, MachineId, PreparationEvent, PreparationInput, PreparedDeploy } from "@ployz/sdk";
import { eq } from "drizzle-orm";
import { Effect, Layer, Schema } from "effect";
import { Inngest } from "inngest";
import { Header } from "tar";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import * as schema from "#/db/schema";
import { asTestDouble } from "#/lib/test-double";
import { createDefaultServiceHealthcheck, createDefaultServiceRestartPolicy, createGitServiceSource, projectServiceDeploymentConfig } from "#/modules/environment-design/services";
import { GithubApi, GithubObservationError, type GithubJsonRequest } from "#/modules/github/github-observation.api";
import { GITHUB_OIDC_ISSUER, GithubOidcKeys } from "#/modules/github/github-oidc.server";
import { InngestClient } from "#/modules/inngest/client";
import { makeOrganizationRuntimeLayer, type OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { runtimeWatchFrameFixture, runtimeWatchMachineFixture, runtimeWatchMachineObservationFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { AppConfig } from "#/server/config.server";
import type { Database, ReportingDatabase } from "#/server/database.server";
import { encodePublicError } from "#/server/public-error";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";
import { noPairingChanges } from "#/test/organization-runtime";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";
import { loadDeploymentBuildLog } from "./deployment-events.server";
import { createMarkCancelledRowBackedWorkflow, createProcessEnvironmentDeployment } from "./environment-deployment.inngest";
import type { BuildOrder } from "./build-order";
import type { SkipReason } from "./image-build";
import { planImageBuildWalk } from "./build-order.server";
import { checkGithubImageBuild, checkInGithubBuild, recordGithubBuildSteps } from "./github-image-builds.server";

const organizationId = "00000000-0000-4000-8000-000000000801";
const userId = "00000000-0000-4000-8000-000000000802";
const projectId = "00000000-0000-4000-8000-000000000803";
const environmentId = "00000000-0000-4000-8000-000000000804";
const savedId = "00000000-0000-4000-8000-000000000805";
const deploymentId = "00000000-0000-4000-8000-000000000806";
const serviceId = "00000000-0000-4000-8000-000000000807";
const runId = "github-build-run";
const githubRunId = 9001;
const serversNotStarted: SkipReason = { builder: "servers", kind: "not_started", minutes: 3 };
const githubNotStarted: SkipReason = { builder: "github", kind: "not_started", minutes: 3 };
const commit = "a".repeat(40);
const pushed = `sha256:${"e".repeat(64)}`;
const workflowRef = "owner/repo/.github/workflows/ployz-build.yml@refs/heads/main";
const encryption = makeSecretEncryption("test-encryption-secret");
const machine = runtimeWatchMachineFixture("b".repeat(32), "entry");

const signing = crypto.generateKeyPairSync("rsa", { modulusLength: 2048 });
const jwk = { ...signing.publicKey.export({ format: "jwk" }), kid: "key-1" };
/** A GitHub Actions OIDC token for the dispatched run, with `claims` overriding any claim. */
type RunnerClaims = { aud: string; repository_id: string; job_workflow_ref: string; run_id: string; event_name: string };
function oidcToken(claims: Partial<RunnerClaims> = {}, key: crypto.KeyObject = signing.privateKey) {
  const part = (json: string) => Buffer.from(json).toString("base64url");
  const body = `${part(JSON.stringify({ alg: "RS256", kid: "key-1" }))}.${part(JSON.stringify({
    iss: GITHUB_OIDC_ISSUER, aud: "http://localhost:3000", exp: Math.floor(Date.now() / 1000) + 300,
    repository_id: "42", job_workflow_ref: workflowRef, run_id: String(githubRunId), event_name: "workflow_dispatch", ...claims,
  }))}`;
  return `${body}.${crypto.sign("RSA-SHA256", Buffer.from(body), key).toString("base64url")}`;
}
type StepsReport = { from: number; events: { at: number; event: PreparationEvent }[]; platforms?: string[] };
const runnerRequest = (token: string) => new Request("http://localhost:3000/api/builds/x", {
  method: "POST", headers: { authorization: `Bearer ${token}` },
});

/** GitHub and the entry Machine, faked at their Context boundaries. */
type Fake = {
  github: { operation: string; url: string; body?: unknown }[];
  minted: string[];
  /** Whether the Cluster refuses to mint Build Grants (a ployzd without the RPC). */
  mintFails: boolean;
  ended: string[];
  prepared: BuildReceipts[];
  /** The Service's Build Platform Requirement, as the Engine reads it from placement. */
  platforms: string[];
  /** GitHub calls that fail, by operation. */
  githubErrors: Map<string, GithubObservationError>;
  /** Each server build's start limit; `serversQueued` withdraws it unstarted. */
  serverBuilds: (number | undefined)[];
  serversQueued: boolean;
  /** Whether the run ends before GitHub's start limit passes. */
  runEndsBeforeLimit: boolean;
  /** The run's status when Cloud asks GitHub. */
  runStatus: string;
  /** The Cluster's Servers, as its Runtime Watch shows them. */
  machines: Machine[];
  /** Each server build's Preferred Server. */
  preferredMachines: (string | undefined)[];
};

const serverReceipt = {
  fingerprint: "b".repeat(64), machine_id: machine.id,
  image: { reference: `sha256:${"c".repeat(64)}`, tags: [], platforms: ["linux/amd64"], location: "unix:///var/run/docker.sock" },
};
const githubError = (operation: "resolve_repository" | "fetch_workflow" | "dispatch_workflow", status: number, code: "not_found" | "request_failed") =>
  new GithubObservationError({ code, operation, status, retriable: false, message: `GitHub answered ${status}.` });

const header = new Header({ path: "root/Dockerfile", size: 0, mode: 0o644, type: "File" });
header.encode();
const archive = gzipSync(Buffer.concat([Buffer.from(header.block ?? Buffer.alloc(512)), Buffer.alloc(1024)]));

function githubApi(fake: Fake) {
  const responses = new Map([
    ["resolve_repository", { id: 42, full_name: "owner/repo", default_branch: "main" }],
    ["fetch_workflow", { path: ".github/workflows/ployz-build.yml", state: "active" }],
    ["dispatch_workflow", { workflow_run_id: githubRunId, html_url: "https://github.com/owner/repo/actions/runs/9001" }],
    ["cancel_run", {}],
    ["fetch_run", { status: fake.runStatus }],
  ]);
  return {
    json: <S extends Schema.ConstraintDecoder<unknown>>(request: GithubJsonRequest<S>) => {
      fake.github.push({ operation: request.operation, url: request.url, body: request.body });
      const error = fake.githubErrors.get(request.operation);
      if (error) return Effect.fail(error);
      return Schema.decodeUnknownEffect(request.schema)(responses.get(request.operation)).pipe(Effect.orDie);
    },
    archive: () => Effect.succeed(new Response(archive)),
  };
}

/** The `n`th grant the fake Cluster mints. */
const grantId = (n: number) => n.toString(16).padStart(64, "0");

function fakeClient(fake: Fake) {
  return asTestDouble<Client>()({
    mintBuildGrant: async ({ repository }: { repository: string }) => {
      if (fake.mintFails) throw new Error("unknown method MintBuildGrant");
      fake.minted.push(repository);
      return { id: grantId(fake.minted.length), grant: "ployzgrant1:secret", expires_in_seconds: 3600 };
    },
    endBuildGrant: async ({ id }: { id: string }) => {
      fake.ended.push(id);
      return { pushed };
    },
    inspect: async () => asTestDouble<MachineDetails>()({ id: machine.id }),
    buildPlatforms: async () => fake.platforms,
    runtime: { watch: async function* () {
      yield runtimeWatchFrameFixture({ machines: fake.machines.map((observed) => runtimeWatchMachineObservationFixture({ machine: observed })) });
    } },
    build: (input: PreparationInput, options?: BuildOptions) => {
      fake.serverBuilds.push(options?.startWithinMs);
      fake.preferredMachines.push(input.preferred_machine);
      const finished: Promise<BuildOutcome> = Promise.resolve(fake.serversQueued ? { kind: "queued" } : { kind: "built", receipt: serverReceipt });
      return { abort: () => undefined, finished, async *[Symbol.asyncIterator]() {
        yield { Selected: { machine, reason: { kind: "spread" as const }, rejections: [] } };
        yield { Build: { Stage: fake.serversQueued ? "Queued" : "Building" } };
      } };
    },
    prepare: (input: PreparationInput) => {
      fake.prepared.push(input.build_receipts ?? {});
      const outcome = { type: "success" as const, completed: [] };
      const prepared = asTestDouble<PreparedDeploy>()({
        project_name: "production", operations: [], warnings: [], would_remove: [], preserved_volumes: [],
        buildReceipts: input.build_receipts ?? {}, pruneTargets: [], close: () => undefined,
        confirm: () => ({ abort: () => undefined, finished: Promise.resolve(outcome),
          async *[Symbol.asyncIterator]() { yield { type: "outcome" as const, outcome }; } }),
      });
      return { abort: () => undefined, finished: Promise.resolve(prepared), async *[Symbol.asyncIterator]() { yield* []; } };
    },
    close: async () => undefined,
  });
}

describe("Image Builds on GitHub Actions", () => {
  let harness: PostgresTestHarness;
  let fake: Fake;

  beforeAll(async () => {
    harness = await startPostgresTestHarness();
  }, 60_000);
  afterAll(async () => {
    await harness?.stop();
  });

  beforeEach(async () => {
    fake = { github: [], minted: [], mintFails: false, ended: [], prepared: [], platforms: ["linux/amd64"], githubErrors: new Map(), serverBuilds: [], serversQueued: false, runEndsBeforeLimit: false, runStatus: "in_progress", machines: [machine], preferredMachines: [] };
    await harness.pool.query(`
      truncate table environment_saved_state_snapshot, environment, project, "user", organization cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'GitHub builds', 'github-builds');
      insert into "user" (id, email, name) values ('${userId}', 'owner@example.com', 'Owner');
      insert into project (id, organization_id, name, slug) values ('${projectId}', '${organizationId}', 'Cloud', 'cloud');
      insert into environment (id, project_id, organization_id, name, namespace, intent) values (
        '${environmentId}', '${projectId}', '${organizationId}', 'Production', 'production',
        '{"version":1,"environmentSlug":"production","services":[],"volumes":[]}');
      insert into organization_build_order (organization_id, build_order) values ('${organizationId}', 'github-only');
      insert into member (user_id, organization_id) values ('${userId}', '${organizationId}');
      insert into github_installation (user_id, installation_id, account_login, account_type) values ('${userId}', 7, 'owner', 'User');
      insert into github_repository_cache (user_id, installation_id, repository_id, name, full_name, default_branch, private, html_url, repo_updated_at)
        values ('${userId}', 7, 42, 'repo', 'owner/repo', 'main', true, 'https://github.com/owner/repo', now());
    `);
    await harness.db.insert(schema.environmentSavedStateSnapshot).values({
      id: savedId, organizationId, environmentId, actorId: userId, volumeDeletionAuthorizations: [],
      intent: { version: 1, environmentSlug: "production", services: [], volumes: [] },
    });
    await harness.db.insert(schema.environmentDeployment).values({
      id: deploymentId, organizationId, environmentId, savedStateSnapshotId: savedId,
      triggerOrigin: { origin: "manual", actorId: userId }, dispatchRequestedAt: new Date(),
      sourcePins: { [serviceId]: { commitSha: commit } },
    });
    await harness.db.insert(schema.environmentDeploymentSecret).values({ organizationId, environmentDeploymentId: deploymentId });
    await harness.db.insert(schema.serviceLineage).values({ id: serviceId, organizationId, projectId, canonicalName: "api", canonicalSlug: "api" });
    await harness.db.insert(schema.service).values({ id: serviceId, organizationId, projectId, environmentId, lineageId: serviceId, name: "api" });
    await harness.db.insert(schema.environmentNodeConfigSnapshot).values({
      organizationId, environmentDeploymentId: deploymentId, environmentId, nodeType: "service", nodeId: serviceId, nodeLineageId: serviceId,
      config: projectServiceDeploymentConfig({
        source: createGitServiceSource({ repository: "owner/repo", repositoryId: 42, access: { type: "github-installation", installationId: 7 } }),
        privateDns: "api", preDeployCommand: null, startCommand: null,
        healthcheck: createDefaultServiceHealthcheck(), restartPolicy: createDefaultServiceRestartPolicy(),
      }),
    });
  });

  type Services = Database | ReportingDatabase | OrganizationRuntime | AppConfig | GithubApi | GithubOidcKeys | InngestClient | SecretEncryption;
  const run = <A, E>(operation: Effect.Effect<A, E, Services>) => harness.runEffect(operation.pipe(
    Effect.provide(makeOrganizationRuntimeLayer(
      () => Effect.succeed({ kind: "ready", generation: "g", connections: [{ management: "ployz1:test", machine_id: machine.id }] }),
      noPairingChanges,
    ).pipe(Layer.provide(makePloyzLayer({ connect: async () => fakeClient(fake) })))),
    Effect.provide(AppConfig.layer),
    Effect.provideService(GithubApi, githubApi(fake)),
    Effect.provideService(GithubOidcKeys, { keys: Effect.succeed([jwk]) }),
    Effect.provideService(InngestClient, new Inngest({ id: "github-builds" })),
    Effect.provideService(SecretEncryption, encryption),
  ));
  // SAFETY: `run` supplies every service the deployment workflow's activities use.
  const runner = makeInngestEffectRunner(run) as typeof runInngestEffect;
  const imageBuildId = async () => (await harness.db.select().from(schema.environmentDeploymentImageBuild))[0]?.id ?? "";
  const row = async () => (await harness.db.select().from(schema.environmentDeploymentImageBuild))[0];
  const github = async () => (await row())?.github;
  const target = async () => ({ id: await imageBuildId(), deploymentId, serviceId, image: "api", buildIndex: 0 });
  const buildOrder = (order: BuildOrder) => harness.db.update(schema.organizationBuildOrder).set({ buildOrder: order });
  const queued = () => harness.db.update(schema.environmentDeployment).set({ status: "queued" }).where(eq(schema.environmentDeployment.id, deploymentId));
  const buildLog = () => harness.runEffect(loadDeploymentBuildLog({ organizationId, deploymentId, after: 0, limit: 50 }));
  const checkIn = async (token: string) => run(checkInGithubBuild(runnerRequest(token), await imageBuildId()));
  const rejection = async (token: string) => run(Effect.flip(checkInGithubBuild(runnerRequest(token), await imageBuildId())));
  const runnerEvents: StepsReport["events"] = [
    { at: 1_000, event: { Build: { Stage: "Building" } } },
    { at: 2_000, event: { Build: { Step: { id: "s1", name: "RUN make", started: null, completed: null, cached: false, error: null } } } },
    { at: 3_000, event: { Build: { StepOutput: { step: "s1", stderr: false, text: "ok\n" } } } },
  ];
  const post = async (batch: StepsReport) => run(recordGithubBuildSteps(runnerRequest(oidcToken()), await imageBuildId(), JSON.stringify(batch)));
  const refused = async (batch: StepsReport) => run(Effect.flip(recordGithubBuildSteps(runnerRequest(oidcToken()), await imageBuildId(), JSON.stringify(batch))));
  /** The runner's whole report in one batch, ending the build. */
  const report = (platforms: string[]) => post({ from: 0, events: runnerEvents, platforms });
  type MockedSteps = NonNullable<ConstructorParameters<typeof InngestTestEngine>[0]["steps"]>;
  const engine = (steps: MockedSteps = []) => new InngestTestEngine({
    function: createProcessEnvironmentDeployment(new Inngest({ id: "github-builds" }), runner),
    events: [{ name: "environment/deploy.requested", data: { environmentDeploymentId: deploymentId, environmentId } }],
    steps,
    transformCtx: (context) => {
      const ctx = mockCtx(context);
      // @inngest/test hands waitForEvent a lazy promise that inngest 4 then validates as an event,
      // so the run's completion is delivered by replacing the tool rather than mocking the step.
      // A GitHub Builder that isn't last waits its start limit first; the run completes after it.
      const waitForEvent = async (_id: string, options: { timeout?: string | number }) => options.timeout === "3m" && !fake.runEndsBeforeLimit
        ? null : { name: "github/build-run.completed", data: { runId: githubRunId } };
      return { ...ctx, runId, step: { ...ctx.step, waitForEvent: asTestDouble<typeof ctx.step.waitForEvent>()(waitForEvent) } };
    },
  });

  /** Resumes a dispatched build: the dispatch already happened, GitHub being the `index`th Builder. */
  const runCompleted = (index = 0): MockedSteps => [
    { id: `start-github-build-${serviceId}-${index}`, handler: () => ({ kind: "dispatched", runId: githubRunId }) },
  ];
  const dispatch = (index = 0) => engine().executeStep(`start-github-build-${serviceId}-${index}`);

  it("dispatches the build workflow on the default branch with no secret inputs, and records the run", async () => {
    await dispatch();
    const dispatched = fake.github.find(({ operation }) => operation === "dispatch_workflow");
    expect(dispatched).toEqual({
      operation: "dispatch_workflow",
      url: "https://api.github.com/repos/owner/repo/actions/workflows/ployz-build.yml/dispatches",
      body: { ref: "main", return_run_details: true, inputs: { build: await imageBuildId(), cloud: "http://localhost:3000", ployz_version: expect.stringMatching(/^\d+\.\d+\.\d+/), runner: "ubuntu-latest" } },
    });
    expect(await row()).toMatchObject({
      status: "building", builder: "github",
      githubRunId, checkedInAt: null,
      github: { fullName: "owner/repo", workflowRef, reason: "first_in_build_order", grant: null, report: null },
    });
  });

  it("rejects a check-in from another repository, workflow ref, run, or event, and a second use", async () => {
    await dispatch();
    expect(await rejection(oidcToken({ aud: "https://elsewhere.test" }))).toMatchObject({ _tag: "Unauthorized" });
    expect(await rejection(oidcToken({}, crypto.generateKeyPairSync("rsa", { modulusLength: 2048 }).privateKey))).toMatchObject({ _tag: "Unauthorized" });
    expect(await rejection(oidcToken({ repository_id: "43" }))).toMatchObject({ _tag: "Forbidden", message: "The token is for another repository." });
    expect(await rejection(oidcToken({ job_workflow_ref: "owner/repo/.github/workflows/ployz-build.yml@refs/heads/feature" })))
      .toMatchObject({ _tag: "Forbidden", message: "The token is for another workflow or branch." });
    expect(await rejection(oidcToken({ run_id: "9002" }))).toMatchObject({ _tag: "Forbidden", message: "The token is for another run." });
    expect(await rejection(oidcToken({ event_name: "push" }))).toMatchObject({ _tag: "Forbidden", message: "The run was not dispatched by Ployz." });
    expect(fake.minted).toEqual([]);

    const accepted = await checkIn(oidcToken());
    expect(accepted).toMatchObject({ grant: "ployzgrant1:secret", commit, fingerprint: expect.stringMatching(/^[0-9a-f]{64}$/) });
    expect(accepted).toHaveProperty("deployment.snapshots.0.config.privateDns", "api");
    expect(fake.minted).toEqual(["ployz-build/api"]);
    expect(await rejection(oidcToken())).toMatchObject({ _tag: "Conflict" });
    expect(fake.minted).toHaveLength(1);
  });

  it("claims nothing when the Cluster can't mint, so the run checks in again", async () => {
    await dispatch();
    fake.mintFails = true;
    const failed = await rejection(oidcToken());
    expect(failed).toMatchObject({ _tag: "BuildGrantUnavailable" });
    expect(encodePublicError(failed)).toEqual({ _tag: "PublicError", code: "BUILD_GRANT_UNAVAILABLE", message: "Your Cluster could not mint a Build Grant." });
    expect(await row()).toMatchObject({ checkedInAt: null, github: { grant: null } });

    fake.mintFails = false;
    expect(await checkIn(oidcToken())).toMatchObject({ grant: "ployzgrant1:secret" });
    expect(await row()).toMatchObject({ checkedInAt: expect.any(Date), machineId: machine.id, github: { grant: { id: grantId(1) } } });
    expect(await rejection(oidcToken())).toMatchObject({ _tag: "Conflict" });
  });

  it("writes the receipt from the digest the Machine received, shows the runner's steps, and deploys with it", async () => {
    await harness.db.update(schema.environmentDeployment).set({ status: "queued" }).where(eq(schema.environmentDeployment.id, deploymentId));
    // The runner, while Cloud waits for the run to complete: check in, build, report its steps.
    await dispatch();
    await checkIn(oidcToken());
    await report(["linux/amd64"]);
    const output = await engine(runCompleted()).execute();
    expect(output.error).toBeUndefined();
    const built = await row();
    expect(built).toMatchObject({ status: "built", machineId: machine.id, github: { report: { platforms: ["linux/amd64"] } } });
    const receipt = JSON.parse(encryption.decrypt(built?.encryptedReceipt ?? encryption.encrypt("null")));
    expect(receipt).toMatchObject({
      machine_id: machine.id, fingerprint: built?.github?.grant?.fingerprint,
      image: { reference: pushed, platforms: ["linux/amd64"], tags: [`ployz-build/api:ployz-sha256-${"e".repeat(64)}`] },
    });
    expect(fake.ended).toEqual(expect.arrayContaining([grantId(1)]));
    expect(fake.prepared.at(-1)).toEqual({ api: receipt });

    const log = await harness.runEffect(loadDeploymentBuildLog({ organizationId, deploymentId, after: 0, limit: 50 }));
    expect(log.steps.filter((step) => step.image === "api").map((step) => step.name)).toEqual(["Building", "RUN make"]);
    expect(log.output.map((line) => line.text)).toEqual(["ok\n"]);
    // A GitHub build has no Server choice; the log links its run and says why GitHub took it.
    expect(log.imageBuilds).toEqual([{ image: "api", serverChoice: null, github: { runUrl: "https://github.com/owner/repo/actions/runs/9001", reason: "first_in_build_order" }, skips: [] }]);
    // The build ended: no more Build Steps are taken.
    expect(await run(Effect.flip(recordGithubBuildSteps(runnerRequest(oidcToken()), built?.id ?? "", JSON.stringify({ from: 3, events: [] })))))
      .toMatchObject({ _tag: "Conflict" });
  }, 30_000);

  it("shows the runner's Build Steps as they arrive, once each, and only after check-in", async () => {
    await dispatch();
    expect(await refused({ from: 0, events: runnerEvents.slice(0, 1) })).toMatchObject({ _tag: "Conflict" });
    await checkIn(oidcToken());
    expect(await post({ from: 0, events: runnerEvents.slice(0, 2) })).toEqual({ received: 2 });
    expect((await buildLog()).steps.filter((step) => step.image === "api").map((step) => step.name)).toEqual(["Building", "RUN make"]);
    // A retried batch repeats lines already taken; only the new one is filed.
    expect(await post({ from: 1, events: runnerEvents.slice(1) })).toEqual({ received: 3 });
    // A batch that skips lines is refused, naming where to resend from.
    expect(await refused({ from: 5, events: [] })).toMatchObject({ _tag: "Conflict" });
    expect(await post({ from: 3, events: [], platforms: ["linux/amd64"] })).toEqual({ received: 3 });
    const log = await buildLog();
    expect(log.output.map((line) => line.text)).toEqual(["ok\n"]);
    // The last batch ends the build: its open phase closes.
    expect(log.steps.find((step) => step.image === "api" && step.name === "Building")?.completedAt).not.toBeNull();
  });

  it("fails the build when the run ends without pushing", async () => {
    await dispatch();
    await checkIn(oidcToken());
    await report([]);
    const output = await engine(runCompleted()).execute();
    expect(output.error).toEqual(expect.objectContaining({ message: "Image Build failed: api." }));
    expect(await row()).toMatchObject({ status: "failed", failureMessage: "GitHub: the run pushed no image." });
  });

  it("cancels the GitHub run and ends the grant when the attempt is cancelled", async () => {
    await dispatch();
    await checkIn(oidcToken());
    const cancelled = await new InngestTestEngine({
      function: createMarkCancelledRowBackedWorkflow(new Inngest({ id: "github-builds" }), runner),
      events: [{ name: "inngest/function.cancelled", data: { function_id: "process-environment-deployment", run_id: runId } }],
    }).execute();
    expect(cancelled.error).toBeUndefined();
    expect(fake.github).toContainEqual({ operation: "cancel_run", url: "https://api.github.com/repos/owner/repo/actions/runs/9001/cancel", body: undefined });
    expect(fake.ended).toEqual([grantId(1)]);
    expect(await row()).toMatchObject({ status: "cancelled" });
  });

  it.each([
    ["no workflow", () => fake.githubErrors.set("fetch_workflow", githubError("fetch_workflow", 404, "not_found")),
      { builder: "github", kind: "no_workflow", repository: "owner/repo" }, "GitHub: no workflow in owner/repo"],
    ["no permission", () => fake.githubErrors.set("resolve_repository", githubError("resolve_repository", 403, "request_failed")),
      { builder: "github", kind: "no_permission", repository: "owner/repo" }, "GitHub: no permission in owner/repo"],
    ["multi-platform", () => { fake.platforms = ["linux/amd64", "linux/arm64"]; },
      { builder: "github", kind: "multi_platform", platforms: ["amd64", "arm64"] }, "GitHub: needs amd64+arm64"],
    ["dispatch error", () => fake.githubErrors.set("dispatch_workflow", githubError("dispatch_workflow", 422, "request_failed")),
      { builder: "github", kind: "dispatch_failed", message: "GitHub answered 422." }, "GitHub: could not start the build (GitHub answered 422.)"],
  ] satisfies [string, () => void, SkipReason, string][])("skips GitHub at once for %s, and with GitHub only the build fails with that reason", async (_case, arrange, reason, message) => {
    arrange();
    await queued();
    const output = await engine().execute();
    expect(output.error).toEqual(expect.objectContaining({ message: "Image Build failed: api." }));
    expect(await row()).toMatchObject({ status: "failed", failureMessage: message, skips: [reason], builder: "server", github: null });
    expect((await buildLog()).imageBuilds).toEqual([expect.objectContaining({ image: "api", skips: [reason] })]);
  });

  it("builds a single-platform arm64 Service on GitHub's native arm64 runner", async () => {
    fake.platforms = ["linux/arm64"];
    await dispatch();
    expect(fake.github.find(({ operation }) => operation === "dispatch_workflow")?.body).toMatchObject({ inputs: { runner: "ubuntu-24.04-arm" } });
  });

  it("hands a build no GitHub runner started in time to the servers, which wait, and refuses the late runner", async () => {
    await buildOrder("github-then-servers");
    await queued();
    const output = await engine().execute();
    expect(output.error).toBeUndefined();
    expect(await row()).toMatchObject({ status: "built", builder: "server", machineId: machine.id, skips: [githubNotStarted], github: null });
    expect(fake.github).toContainEqual({ operation: "cancel_run", url: "https://api.github.com/repos/owner/repo/actions/runs/9001/cancel", body: undefined });
    // The servers are last, so they wait in the queue without a limit.
    expect(new Set(fake.serverBuilds)).toEqual(new Set([undefined]));
    expect(await rejection(oidcToken())).toMatchObject({ _tag: "NotFound" });
    expect(fake.minted).toEqual([]);
  }, 30_000);

  it("keeps the build on GitHub when the runner checked in before the limit", async () => {
    await buildOrder("github-then-servers");
    await dispatch();
    await checkIn(oidcToken());
    expect(await run(checkGithubImageBuild(await target(), { ended: false, startLimit: true }))).toEqual({ kind: "waiting" });
    expect(await row()).toMatchObject({ status: "building", builder: "github", githubRunId, checkedInAt: expect.any(Date), skips: [] });
    expect(fake.github.map(({ operation }) => operation)).not.toContain("cancel_run");
  });

  it("refuses a check-in once the start limit gave the build away", async () => {
    await buildOrder("github-then-servers");
    await dispatch();
    expect(await run(checkGithubImageBuild(await target(), { ended: false, startLimit: true }))).toEqual({ kind: "skipped", reason: githubNotStarted });
    expect(await rejection(oidcToken())).toMatchObject({ _tag: "NotFound" });
    expect(fake.minted).toEqual([]);
  });

  it("settles a run whose completion no webhook delivered once GitHub says it completed", async () => {
    await dispatch();
    await checkIn(oidcToken());
    await report(["linux/amd64"]);
    expect(await run(checkGithubImageBuild(await target(), { ended: false, startLimit: false }))).toEqual({ kind: "waiting" });
    fake.runStatus = "completed";
    expect(await run(checkGithubImageBuild(await target(), { ended: false, startLimit: false })))
      .toMatchObject({ kind: "settled", result: { status: "built" } });
  });

  it("gives the last Builder's run no start limit, and a started run a budget", async () => {
    await dispatch();
    expect(await run(checkGithubImageBuild(await target(), { ended: false, startLimit: false }))).toEqual({ kind: "waiting" });
    expect(await row()).toMatchObject({ status: "building", skips: [] });
    await checkIn(oidcToken());
    await harness.db.update(schema.environmentDeploymentImageBuild).set({ checkedInAt: new Date(Date.now() - 3 * 60 * 60_000) });
    expect(await run(checkGithubImageBuild(await target(), { ended: false, startLimit: false })))
      .toMatchObject({ kind: "settled", result: { status: "failed" } });
    expect(await row()).toMatchObject({ failureMessage: "GitHub: the run didn't finish within 2 hours." });
    expect(fake.github.map(({ operation }) => operation)).toContain("cancel_run");
  });

  it("moves on when the Workflow run webhook reports the run ended before it checked in", async () => {
    await buildOrder("github-then-servers");
    await queued();
    fake.runEndsBeforeLimit = true;
    const output = await engine().execute();
    expect(output.error).toBeUndefined();
    expect(await row()).toMatchObject({ status: "built", builder: "server", skips: [{ builder: "github", kind: "ended_before_start" }] });
  }, 30_000);

  it("overflows a build the servers still queue past the limit to GitHub", async () => {
    await buildOrder("servers-then-github");
    await queued();
    fake.serversQueued = true;
    await dispatch(1);
    expect(fake.serverBuilds).toEqual([3 * 60_000]);
    expect(await row()).toMatchObject({ status: "building", builder: "github", machineId: null, serverChoice: null, skips: [serversNotStarted] });
    await checkIn(oidcToken());
    await report(["linux/amd64"]);
    // Resuming replays the servers' go from memory; it already skipped.
    const output = await engine([
      { id: `build-image-${serviceId}-0`, handler: () => ({ kind: "skipped", reason: serversNotStarted }) },
      ...runCompleted(1),
    ]).execute();
    expect(output.error).toBeUndefined();
    expect(await row()).toMatchObject({ status: "built", builder: "github" });
    expect((await buildLog()).imageBuilds).toEqual([{
      image: "api", serverChoice: null, github: { runUrl: "https://github.com/owner/repo/actions/runs/9001", reason: "next_in_build_order" }, skips: [serversNotStarted],
    }]);
  }, 30_000);
  describe("a Service's Preferred Builder", () => {
    const fast = runtimeWatchMachineFixture("d".repeat(32), "fast");
    const prefer = (preferredBuilder: string) => harness.db.update(schema.service)
      .set({ policy: { autoDeploy: true, waitForCi: false, watchPaths: [], imageUpdate: { type: "off" }, preferredBuilder: asTestDouble<MachineId>()(preferredBuilder) } });
    /** The Builders a fresh Image Build of the Service walks. */
    const plan = async () => {
      await harness.db.delete(schema.environmentDeploymentImageBuild);
      await harness.db.insert(schema.environmentDeploymentImageBuild).values({ organizationId, deploymentId, serviceId, image: "api", inngestRunId: runId });
      return run(planImageBuildWalk(await target()));
    };

    it("defaults to GitHub first once a repository has the build workflow, and to the servers until then", async () => {
      await harness.db.delete(schema.organizationBuildOrder);
      expect(await plan()).toEqual([{ builder: "servers", reason: "first_in_build_order" }]);
      // The latest Saved State builds from owner/repo through the GitHub App, whose workflow is ready.
      await harness.pool.query(`update environment_saved_state_snapshot set intent = jsonb_set(intent, '{services}',
        '[{"config":{"source":{"type":"git","repository":"owner/repo","repositoryId":42,"access":{"type":"github-installation","installationId":7}}}}]')`);
      expect(await plan()).toEqual([{ builder: "github", reason: "first_in_build_order" }, { builder: "servers", reason: "next_in_build_order" }]);
      fake.githubErrors.set("fetch_workflow", githubError("fetch_workflow", 404, "not_found"));
      expect(await plan()).toEqual([{ builder: "servers", reason: "first_in_build_order" }]);
    });

    it("walks the Build Order alone on Auto", async () => {
      await buildOrder("github-then-servers");
      expect(await plan()).toEqual([{ builder: "github", reason: "first_in_build_order" }, { builder: "servers", reason: "next_in_build_order" }]);
    });

    it("tries GitHub first, then the Build Order without it, and records that GitHub was preferred", async () => {
      await buildOrder("servers-then-github");
      await prefer("github");
      expect(await plan()).toEqual([{ builder: "github", reason: "preferred" }, { builder: "servers", reason: "first_in_build_order" }]);
      await dispatch();
      expect(await github()).toMatchObject({ reason: "preferred" });
    });

    it("asks the Cluster for a preferred Server first, then falls through when it doesn't start in time", async () => {
      fake.machines = [machine, fast];
      await buildOrder("github-only");
      await prefer(fast.id);
      expect(await plan()).toEqual([{ builder: "servers", reason: "preferred", machineId: fast.id }, { builder: "github", reason: "first_in_build_order" }]);
      // The whole walk: the preferred Server's go has a start limit, then GitHub gets it.
      await harness.db.delete(schema.environmentDeploymentImageBuild);
      await queued();
      fake.serversQueued = true;
      await dispatch(1);
      expect(fake.serverBuilds).toEqual([3 * 60_000]);
      expect(fake.preferredMachines).toEqual([fast.id]);
      expect(await row()).toMatchObject({ builder: "github", github: { reason: "first_in_build_order" }, skips: [serversNotStarted] });
    }, 30_000);

    it("goes back to Auto, and says why, when the preferred Server no longer builds or is gone", async () => {
      await buildOrder("github-only");
      await prefer(fast.id);
      fake.machines = [machine, { ...fast, accepts_builds: false }];
      expect(await plan()).toEqual([{ builder: "github", reason: "first_in_build_order" }]);
      expect(await row()).toMatchObject({ skips: [{ builder: "servers", kind: "preferred_unavailable", machineId: fast.id, name: "fast" }] });
      fake.machines = [machine];
      // GitHub only: a gone preferred Server never puts the build on your servers.
      expect(await plan()).toEqual([{ builder: "github", reason: "first_in_build_order" }]);
      expect(await row()).toMatchObject({ skips: [{ builder: "servers", kind: "preferred_unavailable", machineId: fast.id, name: null }] });
      expect((await buildLog()).imageBuilds).toEqual([expect.objectContaining({ skips: [expect.objectContaining({ kind: "preferred_unavailable" })] })]);
    });
  });
});
