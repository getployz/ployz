import crypto from "node:crypto";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Clock, Deferred, Effect, Exit, Fiber, Redacted } from "effect";
import { TestClock } from "effect/testing";
import { expect } from "vitest";
import { startGithubBuilds } from "#/modules/config-store/store-deployment.inngest";
import { recordStoreDeploymentRun, stopStoreDeploymentRun } from "#/modules/config-store/store-deployment.server";
import { planStoreGithubBuilds, startStoreGithubBuilds, type StoreGithubTarget } from "#/modules/config-store/store-github-builds.server";
import { asTestDouble } from "#/lib/test-double";
import { GithubObservationError } from "#/modules/github/github-observation.api";
import { RpcError } from "#/modules/runtime/ployz.server";
import type { EffectRunner } from "#/modules/inngest/run.server";
import type { StoreServices } from "#/modules/config-store/store-deployment.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { GithubAuthority, type GithubBuildWorkflowFile, makeGithubAuthorityLocal } from "#/modules/github/github-authority.server";
import { Database } from "#/server/database.server";
import { fakeGithubApi, fakeGithubAuthority } from "#/test/fake-github";
import { acmeWeb, seedStoreGitService, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000b001";
const PROJECT = "00000000-0000-4000-8000-00000000b002";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000b003";
const SERVICE = "00000000-0000-4000-8000-00000000b004";
const API_SERVICE = "00000000-0000-4000-8000-00000000b005";
const DEPLOYMENT = "00000000-0000-4000-8000-00000000b101";
const HEAD = "c".repeat(40);
const RUN = 9001;
const WORKFLOW = "acme/web/.github/workflows/ployz-build.yml@refs/heads/main";
const here = { project: null, environment: null };
const target = { build: `${DEPLOYMENT}.web`, service: "web", repository: "acme/web", repositoryId: 42, installationId: 7, hasNext: true };
const activeWorkflow: GithubBuildWorkflowFile = { fullName: "acme/web", defaultBranch: "main", state: "active", template: "build", dispatchable: true };
const runUrl = (runId: number) => `https://github.com/acme/web/actions/runs/${runId}`;
const unavailable = () => new RpcError({ code: "unavailable", message: "The Config Store is unavailable; retry the command", details: null });

/** How one build started, when the Store records it. */
const startStoreGithubBuild = (organizationId: string, start: StoreGithubTarget) =>
  startStoreGithubBuilds(organizationId, DEPLOYMENT, [start]).pipe(Effect.map(({ started: [started] }) => started));

/**
 * Inngest's steps: one that succeeded answers from its memo on every replay, and one that failed runs again. With
 * `loseFirst`, the first step runs to its end and Inngest never gets its answer, as when the worker's response is lost.
 */
const memoSteps = ({ loseFirst = false } = {}) => {
  const memo = new Map<string, unknown>();
  let losing = loseFirst;
  return asTestDouble<Parameters<typeof startGithubBuilds>[0]>()({
    run: async <T>(id: string, fn: () => Promise<T>): Promise<T> => {
      if (memo.has(id)) return memo.get(id) as T;
      const value = await fn();
      if (losing) {
        losing = false;
        throw new Error(`Inngest lost the answer of ${id}`);
      }
      memo.set(id, value);
      return value;
    },
  });
};

/** Cloud with `acme/web`'s web Service admitted and pinned, whose GitHub dispatches through `authority`. */
const pinnedWeb = Effect.fn(function* (authority: ReturnType<typeof fakeGithubAuthority>, env?: Record<string, string>, withApi = false) {
  const api = fakeGithubApi({ "/repos/acme/web": { id: 42, full_name: "acme/web" } });
  const services = yield* storeTestCloud({ github: api.service, authority: authority.service, ...(env && { env }) });
  const provided = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(Effect.provide(services));
  yield* provided(seedStoreOrganization(ORGANIZATION));
  const store = yield* provided(seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }));
  if (withApi) {
    yield* Effect.promise(() => store.write(ORGANIZATION, {
      command: "create_git_service", id: API_SERVICE, environment: here, name: "api", repository: "acme/web", branch: null,
    }, acmeWeb()));
  }
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "admit", admit: "deploy", id: DEPLOYMENT, environment: here, services: [], version: null, accept_volume_loss: [],
  }, acmeWeb()));
  yield* Effect.promise(() => store.pinSources(DEPLOYMENT, withApi ? { web: HEAD, api: HEAD } : { web: HEAD }));
  const cloud = yield* provided(cloudStore);
  cloud.githubStart = async () => ({ kind: "dispatch", runner: "ubuntu-latest", foreign: [] });
  const web = () => Effect.promise(() => store.deploymentSources(DEPLOYMENT)).pipe(Effect.map((sources) => sources.find(({ service }) => service === "web")));
  return { services, provided, web };
});

/** Another writer holding the Environment's row, so the Store's writes to it wait and then give up, until `release`. */
const holdEnvironment = Effect.fn(function* (provided: (program: Effect.Effect<void, unknown, Database>) => Effect.Effect<void, unknown>) {
  const locked = yield* Deferred.make<void>();
  const release = yield* Deferred.make<void>();
  const holder = yield* provided(Effect.gen(function* () {
    const database = yield* Database;
    yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.execute(sql`select id from config_environment where id = ${ENVIRONMENT} for update`, "objects");
      yield* Deferred.succeed(locked, undefined);
      yield* Deferred.await(release);
    }));
  })).pipe(Effect.forkChild);
  yield* Deferred.await(locked);
  return Deferred.succeed(release, undefined).pipe(Effect.andThen(Fiber.join(holder)), Effect.asVoid);
});

it.live("a run GitHub took while the Store can't record it is recorded once the Store can, and no other run is dispatched", () =>
  Effect.gen(function* () {
    const dispatched: number[] = [];
    const authority = fakeGithubAuthority({
      buildWorkflow: () => Effect.succeed(activeWorkflow),
      dispatchBuild: () => Effect.sync(() => {
        const runId = RUN + dispatched.length;
        dispatched.push(runId);
        return { runId, runUrl: runUrl(runId), headSha: HEAD, workflowRef: WORKFLOW };
      }),
      cancelBuild: () => Effect.void,
    });
    const { provided, web } = yield* pinnedWeb(authority);
    const cloud = yield* provided(cloudStore);
    const refused = yield* Deferred.make<void>();
    const recordRun = cloud.githubDispatched.bind(cloud);
    cloud.githubDispatched = async (build, run) => {
      try {
        return await recordRun(build, run);
      } catch (refusal) {
        Effect.runSync(Deferred.succeed(refused, undefined));
        throw refusal;
      }
    };
    // Another writer holds the Environment's row until the Store has refused to record the run once.
    const release = yield* holdEnvironment(provided);
    const starting = yield* provided(startStoreGithubBuild(ORGANIZATION, target)).pipe(Effect.forkChild);
    yield* Deferred.await(refused);
    yield* release;
    expect(yield* Fiber.join(starting)).toEqual({ kind: "dispatched", runId: RUN });
    expect(yield* web()).toMatchObject({ status: "building" });
    expect(dispatched).toEqual([RUN]);
    expect(authority.calls.filter(({ operation }) => operation === "cancelBuild")).toEqual([]);
  }), 60_000);

it.live("a start whose answer Inngest lost after GitHub took each build runs again without asking GitHub for any of them", () =>
  Effect.gen(function* () {
    const dispatched: string[] = [];
    const authority = fakeGithubAuthority({
      buildWorkflow: () => Effect.succeed(activeWorkflow),
      dispatchBuild: ({ inputs: { build } }) => Effect.sync(() => {
        const runId = RUN + dispatched.length;
        dispatched.push(build);
        return { runId, runUrl: runUrl(runId), headSha: HEAD, workflowRef: WORKFLOW };
      }),
      cancelBuild: () => Effect.void,
    });
    const { provided } = yield* pinnedWeb(authority, undefined, true);
    const api = { ...target, service: "api", build: `${DEPLOYMENT}.api` };
    const data = { organizationId: ORGANIZATION, environmentId: ENVIRONMENT, deploymentId: DEPLOYMENT, trigger: "user" as const };
    const runEffect: EffectRunner<StoreServices> = (program) => Effect.runPromise(provided(program));
    const steps = memoSteps({ loseFirst: true });
    const start = Effect.tryPromise(() => startGithubBuilds(steps, runEffect, data, [target, api]));

    // GitHub took both builds, and Inngest never got the step's answer, so it runs the function again.
    expect(Exit.isFailure(yield* Effect.exit(start))).toBe(true);
    expect(yield* start).toEqual([{ kind: "dispatched", runId: RUN }, { kind: "dispatched", runId: RUN + 1 }]);
    expect(dispatched).toEqual([target.build, api.build]);
  }), 60_000);

it.effect.each(["answered", "stalled"] as const)(
  "a build GitHub was asked for is never asked for again, however long the Store refuses its record (GitHub %s)",
  (answer) => Effect.gen(function* () {
    const dispatched: number[] = [];
    const authority = fakeGithubAuthority({
      buildWorkflow: () => Effect.succeed(activeWorkflow),
      dispatchBuild: () => Effect.gen(function* () {
        const runId = RUN + dispatched.length;
        dispatched.push(runId);
        // The 30-second bound ends a stalled answer as a failed call.
        if (answer === "stalled") {
          return yield* new GithubObservationError({ code: "request_failed", operation: "dispatch_workflow", retriable: true, message: "GitHub did not answer" });
        }
        return { runId, runUrl: runUrl(runId), headSha: HEAD, workflowRef: WORKFLOW };
      }),
      cancelBuild: () => Effect.void,
    });
    const { provided, web } = yield* pinnedWeb(authority);
    const clock = yield* Clock.Clock;
    // The Store refuses every record while another writer holds the Environment, past every retry the steps make.
    let locked = true;
    let refusals = 0;
    const cloud = yield* provided(cloudStore);
    const [recordRun, recordSkip] = [cloud.githubDispatched.bind(cloud), cloud.githubSkip.bind(cloud)];
    const refuse = () => {
      refusals += 1;
      return Promise.reject(unavailable());
    };
    cloud.githubDispatched = (build, run) => locked ? refuse() : recordRun(build, run);
    cloud.githubSkip = (build, message) => locked ? refuse() : recordSkip(build, message);
    const steps = memoSteps();
    const data = { organizationId: ORGANIZATION, environmentId: ENVIRONMENT, deploymentId: DEPLOYMENT, trigger: "user" as const };
    const runEffect: EffectRunner<StoreServices> = (program) =>
      Effect.runPromise(provided(program).pipe(Effect.provideService(Clock.Clock, clock)));
    // Inngest runs the function again after each failed try; the record's own retries wait on the test's clock.
    const run = Effect.gen(function* () {
      const trying = yield* Effect.promise(() => startGithubBuilds(steps, runEffect, data, [target])).pipe(Effect.exit, Effect.forkChild);
      while (trying.pollUnsafe() === undefined) yield* TestClock.adjust("1 second");
      return yield* Fiber.join(trying);
    });

    for (let attempt = 0; attempt < 4; attempt += 1) expect(Exit.isFailure(yield* run)).toBe(true);
    // Each record is refused six times: in the start step on the first try, then in its own step on every try.
    expect(refusals).toBe(30);
    expect(dispatched).toEqual([RUN]);
    locked = false;
    const started = yield* run;
    expect(started).toEqual(Exit.succeed(answer === "answered" ? [{ kind: "dispatched", runId: RUN }] : [{ kind: "done" }]));
    expect(yield* web()).toMatchObject(answer === "answered" ? { status: "building" } : { status: "pending", message: expect.stringContaining("GitHub couldn't take the build") });
    expect(yield* provided(planStoreGithubBuilds(data))).toEqual([]);
    expect(dispatched).toEqual([RUN]);
  }),
  60_000,
);

it.effect("a run whose record the Store refuses on every try is cancelled before the Deployment fails", () =>
  Effect.gen(function* () {
    const dispatched: number[] = [];
    const cancelled: number[] = [];
    const authority = fakeGithubAuthority({
      buildWorkflow: () => Effect.succeed(activeWorkflow),
      dispatchBuild: () => Effect.sync(() => {
        const runId = RUN + dispatched.length;
        dispatched.push(runId);
        return { runId, runUrl: runUrl(runId), headSha: HEAD, workflowRef: WORKFLOW };
      }),
      cancelBuild: ({ runId }) => Effect.sync(() => void cancelled.push(runId)),
    });
    const { provided, web } = yield* pinnedWeb(authority);
    const clock = yield* Clock.Clock;
    let locked = true;
    const cloud = yield* provided(cloudStore);
    const recordRun = cloud.githubDispatched.bind(cloud);
    cloud.githubDispatched = (build, run) => locked ? Promise.reject(unavailable()) : recordRun(build, run);
    const steps = memoSteps();
    const data = { organizationId: ORGANIZATION, environmentId: ENVIRONMENT, deploymentId: DEPLOYMENT, trigger: "user" as const };
    const runEffect: EffectRunner<StoreServices> = (program) =>
      Effect.runPromise(provided(program).pipe(Effect.provideService(Clock.Clock, clock)));
    const run = (last: boolean) => Effect.gen(function* () {
      const trying = yield* Effect.promise(() => startGithubBuilds(steps, runEffect, data, [target], last)).pipe(Effect.exit, Effect.forkChild);
      while (trying.pollUnsafe() === undefined) yield* TestClock.adjust("1 second");
      return yield* Fiber.join(trying);
    });
    yield* provided(recordStoreDeploymentRun("worker-run", data));

    // Inngest's three tries; only the last gives the run up.
    for (const last of [false, false, true]) {
      expect(Exit.isFailure(yield* run(last))).toBe(true);
      expect(cancelled).toEqual(last ? [RUN] : []);
    }
    // The Store is back for the function's failure, which stops only the runs it recorded.
    locked = false;
    yield* provided(stopStoreDeploymentRun(ORGANIZATION, DEPLOYMENT, "worker-run"));
    expect(dispatched).toEqual([RUN]);
    expect(cancelled).toEqual([RUN]);
    expect(yield* web()).toMatchObject({ status: "pending" });
  }),
  60_000,
);

it.effect.each([false, true])(
  "a dispatch whose answer stalls ends inside its bound, and the build goes to the next Builder (Environment locked: %s)",
  (locked) => Effect.gen(function* () {
    const dispatched: number[] = [];
    const taken = yield* Deferred.make<void>();
    const closed = yield* Deferred.make<void>();
    // GitHub takes the dispatch, starts its answer, and never finishes it.
    const github = createServer(async (request, response) => {
      const path = request.url ?? "";
      response.setHeader("content-type", "application/json");
      if (path.endsWith("/access_tokens")) return response.end(JSON.stringify({ token: "local", expires_at: "2099-01-01T00:00:00Z" }));
      if (path.endsWith("/git/ref/heads/main")) return response.end(JSON.stringify({ object: { type: "commit", sha: HEAD } }));
      if (path === "/repositories/42") return response.end(JSON.stringify({ full_name: "acme/web", default_branch: "main" }));
      if (!path.endsWith("/dispatches")) {
        response.statusCode = 404;
        return response.end(JSON.stringify({}));
      }
      for await (const _ of request);
      dispatched.push(RUN + dispatched.length);
      response.on("close", () => Effect.runSync(Deferred.succeed(closed, undefined)));
      response.write('{"workflow_run_id":');
      Effect.runSync(Deferred.succeed(taken, undefined));
    });
    yield* Effect.acquireRelease(
      Effect.promise(() => new Promise<void>((resolve) => github.listen(0, "127.0.0.1", resolve))),
      () => Effect.sync(() => { github.closeAllConnections(); github.close(); }),
    );
    const address = github.address() as AddressInfo;
    const authority = fakeGithubAuthority({ buildWorkflow: () => Effect.succeed(activeWorkflow), cancelBuild: () => Effect.void });
    const { services, provided, web } = yield* pinnedWeb(authority, { PLOYZ_VERIFY_GITHUB_ORIGIN: `http://127.0.0.1:${address.port}` });
    const { privateKey } = crypto.generateKeyPairSync("rsa", { modulusLength: 2048 });
    const local = yield* makeGithubAuthorityLocal({
      appPrivateKey: Redacted.make(privateKey.export({ type: "pkcs8", format: "pem" })),
      appClientSecret: Redacted.make("local"),
    }).pipe(Effect.provide(services));
    // The test's clock, which the built Cloud would otherwise replace with the live one.
    const clock = yield* Clock.Clock;
    // Locked, the Store refuses the first record of the skip, and lets the next one through.
    const cloud = yield* provided(cloudStore);
    const skipRefused = yield* Deferred.make<void>();
    const recordSkip = cloud.githubSkip.bind(cloud);
    cloud.githubSkip = async (build, message) => {
      try {
        return await recordSkip(build, message);
      } catch (refusal) {
        Effect.runSync(Deferred.succeed(skipRefused, undefined));
        throw refusal;
      }
    };
    const starting = yield* provided(startStoreGithubBuild(ORGANIZATION, target).pipe(
      Effect.updateService(GithubAuthority, (fake) => ({ ...fake, dispatchBuild: local.dispatchBuild })),
    )).pipe(Effect.provideService(Clock.Clock, clock), Effect.forkChild);
    yield* Deferred.await(taken);
    if (locked) {
      const release = yield* holdEnvironment(provided);
      yield* TestClock.adjust("30 seconds");
      yield* Deferred.await(skipRefused);
      yield* release;
    } else {
      yield* TestClock.adjust("30 seconds");
    }
    // The skip's retries wait on the test's clock.
    while (starting.pollUnsafe() === undefined) yield* TestClock.adjust("1 second");
    expect(yield* Fiber.join(starting)).toEqual({ kind: "done" });
    expect(yield* web()).toMatchObject({ status: "pending", message: expect.stringContaining("GitHub couldn't take the build") });
    // The build is no longer GitHub's to start, so nothing dispatches it again.
    expect(yield* provided(planStoreGithubBuilds({ organizationId: ORGANIZATION, environmentId: ENVIRONMENT, deploymentId: DEPLOYMENT, trigger: "user" }))).toEqual([]);
    expect(dispatched).toHaveLength(1);
    // Cloud gave up on the answer, so it closes the connection rather than holding it open.
    yield* Deferred.await(closed);
  }),
  60_000,
);

it.effect("a workflow file whose body stalls fails inside its bound and closes its connection", () =>
  Effect.gen(function* () {
    const taken = yield* Deferred.make<void>();
    const closed = yield* Deferred.make<void>();
    // GitHub starts sending the workflow file and never finishes it.
    const github = createServer((request, response) => {
      const path = request.url ?? "";
      response.setHeader("content-type", "application/json");
      if (path.endsWith("/access_tokens")) return response.end(JSON.stringify({ token: "local", expires_at: "2099-01-01T00:00:00Z" }));
      if (path === "/repositories/42") return response.end(JSON.stringify({ full_name: "acme/web", default_branch: "main" }));
      if (path.includes("/actions/workflows/")) return response.end(JSON.stringify({ path: ".github/workflows/ployz-build.yml", state: "active" }));
      if (path.includes("/contents/")) {
        response.on("close", () => Effect.runSync(Deferred.succeed(closed, undefined)));
        response.write("name: Ployz build");
        Effect.runSync(Deferred.succeed(taken, undefined));
        return;
      }
      response.statusCode = 404;
      response.end();
    });
    yield* Effect.acquireRelease(
      Effect.promise(() => new Promise<void>((resolve) => github.listen(0, "127.0.0.1", resolve))),
      () => Effect.sync(() => { github.closeAllConnections(); github.close(); }),
    );
    const address = github.address() as AddressInfo;
    const { services } = yield* pinnedWeb(fakeGithubAuthority({}), { PLOYZ_VERIFY_GITHUB_ORIGIN: `http://127.0.0.1:${address.port}` });
    const { privateKey } = crypto.generateKeyPairSync("rsa", { modulusLength: 2048 });
    const local = yield* makeGithubAuthorityLocal({
      appPrivateKey: Redacted.make(privateKey.export({ type: "pkcs8", format: "pem" })),
      appClientSecret: Redacted.make("local"),
    }).pipe(Effect.provide(services));
    const reading = yield* local.buildWorkflow({ installationId: 7, repositoryId: 42 }).pipe(Effect.flip, Effect.forkChild);
    yield* Deferred.await(taken);
    yield* TestClock.adjust("30 seconds");
    expect(yield* Fiber.join(reading)).toMatchObject({ operation: "fetch_contents", code: "request_failed" });
    // Cloud gave up on the file, so it closes the connection rather than holding it open.
    yield* Deferred.await(closed);
  }), 15_000);
