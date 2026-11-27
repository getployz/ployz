import crypto from "node:crypto";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Clock, Deferred, Effect, Exit, Fiber, Redacted, Schema } from "effect";
import { TestClock } from "effect/testing";
import { expect, vi } from "vitest";
import { InngestTestEngine } from "@inngest/test";
import { Inngest } from "inngest";
import { createRunStoreDeployment, startGithubBuilds } from "#/modules/config-store/store-deployment.inngest";
import { createStopStoreDeployment } from "#/modules/config-store/store-deployment-stop.inngest";
import { configDeploymentAdmitted, configDeploymentStopRequested } from "#/modules/config-store/config-store.events";
import { failureSteps, invokeOnFailure } from "#/test/inngest-failure";
import { recordStoreDeploymentRun, staleStoreDeploymentRuns, stopStoreDeploymentRun } from "#/modules/config-store/store-deployment.server";
import { cancelStoreGithubBuilds, planStoreGithubBuilds, recordStoreGithubBuild, startStoreGithubBuilds, type StoreGithubTarget } from "#/modules/config-store/store-github-builds.server";
import { asTestDouble } from "#/lib/test-double";
import { GithubObservationError } from "#/modules/github/github-observation.api";
import type { ConfigStore } from "@ployz/sdk";
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

/** The worker run that starts the builds. */
const ATTEMPT = "cloud-worker-run";

/** How one build started, when the Store records it. */
const startStoreGithubBuild = (organizationId: string, start: StoreGithubTarget) =>
  startStoreGithubBuilds(organizationId, DEPLOYMENT, [start], ATTEMPT).pipe(Effect.map(({ started: [started] }) => started));

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
    const [taken, locked] = [yield* Deferred.make<void>(), yield* Deferred.make<void>()];
    const authority = fakeGithubAuthority({
      buildWorkflow: () => Effect.succeed(activeWorkflow),
      dispatchBuild: () => Effect.gen(function* () {
        const runId = RUN + dispatched.length;
        dispatched.push(runId);
        yield* Deferred.succeed(taken, undefined);
        yield* Deferred.await(locked);
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
    // Once GitHub took the run, another writer holds the Environment's row until the Store has refused to record it once.
    const starting = yield* provided(startStoreGithubBuild(ORGANIZATION, target)).pipe(Effect.forkChild);
    yield* Deferred.await(taken);
    const release = yield* holdEnvironment(provided);
    yield* Deferred.succeed(locked, undefined);
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
    const start = Effect.tryPromise(() => startGithubBuilds(steps, runEffect, data, [target, api], ATTEMPT));

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
      const trying = yield* Effect.promise(() => startGithubBuilds(steps, runEffect, data, [target], ATTEMPT)).pipe(Effect.exit, Effect.forkChild);
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
      const trying = yield* Effect.promise(() => startGithubBuilds(steps, runEffect, data, [target], ATTEMPT, last)).pipe(Effect.exit, Effect.forkChild);
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

it.live("a push's own run is left alone when the Store refuses its record", () =>
  Effect.gen(function* () {
    const cancelled: number[] = [];
    const authority = fakeGithubAuthority({
      buildWorkflow: () => Effect.succeed({ ...activeWorkflow, template: "rule" }),
      buildRuns: () => Effect.succeed([{ id: RUN, url: runUrl(RUN), headSha: HEAD, event: "push", status: "in_progress", conclusion: null }]),
      cancelBuild: ({ runId }) => Effect.sync(() => void cancelled.push(runId)),
    });
    const { provided } = yield* pinnedWeb(authority);
    const cloud = yield* provided(cloudStore);
    cloud.githubStart = async () => ({ kind: "follow", workflow: ".github/workflows/ployz-build.yml", workflow_ref: WORKFLOW, commit: HEAD, foreign: [] });
    // The build is passed on just before the push's run is recorded, so the Store refuses the record.
    const recordRun = cloud.githubDispatched.bind(cloud);
    cloud.githubDispatched = async (build, run) => {
      await cloud.githubSkip(build, "passed on meanwhile");
      return recordRun(build, run);
    };
    const data = { organizationId: ORGANIZATION, environmentId: ENVIRONMENT, deploymentId: DEPLOYMENT, trigger: "user" as const };
    const runEffect: EffectRunner<StoreServices> = (program) => Effect.runPromise(provided(program));
    expect(yield* Effect.promise(() => startGithubBuilds(memoSteps(), runEffect, data, [target], ATTEMPT))).toEqual([{ kind: "done" }]);
    expect(cancelled).toEqual([]);
  }),
  60_000,
);

it.effect.each(["committed", "late", "unanswered"] as const)(
  "a last try whose record the Store may still have taken never cancels the run that holds the build (record %s)",
  (write) => Effect.gen(function* () {
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
    const cloud = yield* provided(cloudStore);
    const recordRun = cloud.githubDispatched.bind(cloud);
    // Every record answers unavailable. The last one either committed anyway, or commits only after the step gave up.
    let writes = 0;
    cloud.githubDispatched = async (build, run) => {
      writes += 1;
      if (writes === 24 && write === "committed") await recordRun(build, run);
      throw unavailable();
    };
    // With "unanswered", the Store doesn't answer the pass-on either, so nothing proves the record absent.
    if (write === "unanswered") cloud.githubSkip = () => Promise.reject(unavailable());
    const steps = memoSteps();
    const data = { organizationId: ORGANIZATION, environmentId: ENVIRONMENT, deploymentId: DEPLOYMENT, trigger: "user" as const };
    const runEffect: EffectRunner<StoreServices> = (program) =>
      Effect.runPromise(provided(program).pipe(Effect.provideService(Clock.Clock, clock)));
    const run = (last: boolean) => Effect.gen(function* () {
      const trying = yield* Effect.promise(() => startGithubBuilds(steps, runEffect, data, [target], ATTEMPT, last)).pipe(Effect.exit, Effect.forkChild);
      while (trying.pollUnsafe() === undefined) yield* TestClock.adjust("1 second");
      return yield* Fiber.join(trying);
    });
    for (const last of [false, false, true]) expect(Exit.isFailure(yield* run(last))).toBe(true);
    expect(writes).toBe(24);
    if (write === "committed") {
      // The record landed: the run holds the build and keeps running.
      expect(yield* web()).toMatchObject({ status: "building" });
      expect(cancelled).toEqual([]);
    } else if (write === "unanswered") {
      expect(cancelled).toEqual([]);
    } else {
      // The record commits only after the step gave up. The build was passed on first, so it is refused.
      const late = yield* Effect.exit(Effect.tryPromise(() => recordRun(target.build, {
        run_id: RUN, run_url: runUrl(RUN), workflow_ref: WORKFLOW, repository: "acme/web", installation_id: 7,
      })));
      expect(Exit.isFailure(late)).toBe(true);
      expect(yield* web()).toMatchObject({ status: "pending", message: "GitHub's run couldn't be recorded" });
      expect(cancelled).toEqual([RUN]);
    }
    expect(dispatched).toEqual([RUN]);
  }),
  60_000,
);

/** Until `count` Store writes wait on a lock, as behind `holdEnvironment`. */
const waitingOnLocks = Effect.fn(function* (provided: (program: Effect.Effect<number, unknown, Database>) => Effect.Effect<number, unknown>, count: number) {
  const waiting = provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.execute<{ waiting: number }>(
      sql`select count(*)::int as waiting from pg_stat_activity where wait_event_type = 'Lock' and datname = current_database()`, "objects");
    return row?.waiting ?? 0;
  }));
  yield* Effect.promise(() => vi.waitFor(async () => expect(await Effect.runPromise(waiting)).toBeGreaterThanOrEqual(count), { timeout: 10_000 }));
});

/** The run holding `build` now, if any. */
const holderOf = (cloud: Pick<ConfigStore, "githubBuild">, build: string) =>
  Effect.tryPromise(() => cloud.githubBuild(build)).pipe(
    Effect.map((found): number | null => "held" in found ? found.held.run.run_id : null),
    Effect.orElseSucceed(() => null),
  );

it.live("a record that commits after its caller gave up is cancelled once the Deployment fails", () =>
  Effect.gen(function* () {
    const cancelled: number[] = [];
    const authority = fakeGithubAuthority({ cancelBuild: ({ runId }) => Effect.sync(() => void cancelled.push(runId)) });
    const { provided } = yield* pinnedWeb(authority);
    const cloud = yield* provided(cloudStore);
    const data = { organizationId: ORGANIZATION, environmentId: ENVIRONMENT, deploymentId: DEPLOYMENT, trigger: "user" as const };
    yield* provided(recordStoreDeploymentRun("worker-run", data));
    // Another writer holds the Environment, so the run's record waits; its caller has stopped waiting, as the SDK does
    // at its bound, and Cloud's give-up got no answer either, so it left the run alone. The write commits whenever the
    // writer lets go, however late.
    const release = yield* holdEnvironment(provided);
    const landed = cloud.githubDispatched(target.build, {
      run_id: RUN, run_url: runUrl(RUN), workflow_ref: WORKFLOW, repository: "acme/web", installation_id: 7, push: false, attempt: ATTEMPT,
    }).then(() => "committed" as const, () => "refused" as const);
    yield* waitingOnLocks(provided, 1);
    // The function failed, and its cleanup starts while the record still waits. Then the writer lets go.
    const cleanup = yield* provided(stopStoreDeploymentRun(ORGANIZATION, DEPLOYMENT, "worker-run")).pipe(Effect.exit, Effect.forkChild);
    yield* waitingOnLocks(provided, 2);
    yield* release;
    yield* Fiber.join(cleanup);
    expect(yield* Effect.promise(() => landed)).toBe("committed");
    // The run the late record made the holder is cancelled with the Deployment.
    expect(yield* holderOf(cloud, target.build)).toBe(RUN);
    expect(cancelled).toEqual([RUN]);
  }),
  60_000,
);

it.live("a replay's record that waits on a late record follows the run that took the build and cancels its own", () =>
  Effect.gen(function* () {
    const cancelled: number[] = [];
    const authority = fakeGithubAuthority({ cancelBuild: ({ runId }) => Effect.sync(() => void cancelled.push(runId)) });
    const { provided } = yield* pinnedWeb(authority);
    const cloud = yield* provided(cloudStore);
    const run = (runId: number) => ({ run_id: runId, run_url: runUrl(runId), workflow_ref: WORKFLOW, repository: "acme/web", installation_id: 7, push: false, attempt: ATTEMPT });
    // The lost start's record of 9001 still waits on the Environment when the replay, which asked GitHub again, records
    // 9002.
    const release = yield* holdEnvironment(provided);
    const landed = cloud.githubDispatched(target.build, run(RUN)).then(() => "committed" as const, () => "refused" as const);
    yield* waitingOnLocks(provided, 1);
    const replay = yield* provided(recordStoreGithubBuild({ kind: "run", build: target.build, run: run(RUN + 1) })).pipe(Effect.forkChild);
    yield* waitingOnLocks(provided, 2);
    yield* release;
    expect(yield* Fiber.join(replay)).toEqual({ kind: "dispatched", runId: RUN });
    expect(yield* Effect.promise(() => landed)).toBe("committed");
    expect(yield* holderOf(cloud, target.build)).toBe(RUN);
    expect(cancelled).toEqual([RUN + 1]);
  }),
  60_000,
);

it.live("a replay that starts while the first try's record still commits asks GitHub for nothing and follows that try's run", () =>
  Effect.gen(function* () {
    const dispatched: number[] = [];
    const cancelled: number[] = [];
    const [taken, locked] = [yield* Deferred.make<void>(), yield* Deferred.make<void>()];
    const authority = fakeGithubAuthority({
      buildWorkflow: () => Effect.succeed(activeWorkflow),
      dispatchBuild: () => Effect.gen(function* () {
        const runId = RUN + dispatched.length;
        dispatched.push(runId);
        if (runId === RUN) {
          yield* Deferred.succeed(taken, undefined);
          yield* Deferred.await(locked);
        }
        return { runId, runUrl: runUrl(runId), headSha: HEAD, workflowRef: WORKFLOW };
      }),
      cancelBuild: ({ runId }) => Effect.sync(() => void cancelled.push(runId)),
    });
    const { provided } = yield* pinnedWeb(authority);
    const cloud = yield* provided(cloudStore);
    // GitHub took the first try's run, whose record then waits on the Environment. Its caller gave up, as the SDK does
    // at its bound, so Inngest runs the step again while that record may still commit.
    const first = yield* provided(startStoreGithubBuild(ORGANIZATION, target)).pipe(Effect.forkChild);
    yield* Deferred.await(taken);
    const release = yield* holdEnvironment(provided);
    yield* Deferred.succeed(locked, undefined);
    yield* waitingOnLocks(provided, 1);
    const replay = yield* provided(startStoreGithubBuild(ORGANIZATION, target)).pipe(Effect.forkChild);
    yield* waitingOnLocks(provided, 2);
    yield* release;
    expect(yield* Fiber.join(replay)).toEqual({ kind: "dispatched", runId: RUN });
    expect(yield* Fiber.join(first)).toEqual({ kind: "dispatched", runId: RUN });
    expect(yield* holderOf(cloud, target.build)).toBe(RUN);
    expect(dispatched).toEqual([RUN]);
    expect(cancelled).toEqual([]);
  }),
  60_000,
);

const dispatchedRun = (runId: number) => ({ run_id: runId, run_url: runUrl(runId), workflow_ref: WORKFLOW, repository: "acme/web", installation_id: 7, push: false, attempt: ATTEMPT });
const failedData = { organizationId: ORGANIZATION, environmentId: ENVIRONMENT, deploymentId: DEPLOYMENT, trigger: "user" as const };

it.live("a failed run whose stop gets no answer cancels nothing, and the stop it hands on cancels the run once the Store answers", () =>
  Effect.gen(function* () {
    const cancelled: number[] = [];
    const authority = fakeGithubAuthority({ cancelBuild: ({ runId }) => Effect.sync(() => void cancelled.push(runId)) });
    const { provided } = yield* pinnedWeb(authority);
    const cloud = yield* provided(cloudStore);
    yield* provided(recordStoreDeploymentRun("worker-run", failedData));
    yield* Effect.promise(() => cloud.githubDispatched(target.build, dispatchedRun(RUN)));
    const runEffect: EffectRunner<StoreServices> = (program) => Effect.runPromise(provided(program));
    const worker = createRunStoreDeployment(new Inngest({ id: "stop-test" }), runEffect);
    // Another writer holds the Environment past the Store's lock timeout, so the stop gets no answer.
    const release = yield* holdEnvironment(provided);
    const steps = failureSteps();
    yield* Effect.exit(Effect.tryPromise(() => invokeOnFailure(worker, "worker-run", { name: configDeploymentAdmitted.name, data: failedData }, steps)));
    const unanswered = [...cancelled];
    yield* release;
    expect(unanswered).toEqual([]);
    expect(steps.sent).toMatchObject([{ name: configDeploymentStopRequested.name, data: { runId: "worker-run", round: 0 } }]);
    const { data } = Schema.decodeUnknownSync(Schema.Struct({ data: configDeploymentStopRequested.schema }))(steps.sent[0]);
    const { result } = yield* Effect.promise(() => new InngestTestEngine({
      function: createStopStoreDeployment(new Inngest({ id: "stop-test" }), runEffect),
      events: [{ name: configDeploymentStopRequested.name, data }],
      steps: [{ id: "wait-0", handler: () => null }],
    }).execute());
    expect(result).toEqual({ failed: DEPLOYMENT });
    expect(cancelled).toEqual([RUN]);
  }),
  60_000,
);

it.live("a stop whose build's end gets no answer leaves that run alone and cancels it on the next try", () =>
  Effect.gen(function* () {
    const cancelled: number[] = [];
    const authority = fakeGithubAuthority({ cancelBuild: ({ runId }) => Effect.sync(() => void cancelled.push(runId)) });
    const { provided } = yield* pinnedWeb(authority);
    const cloud = yield* provided(cloudStore);
    yield* provided(recordStoreDeploymentRun("worker-run", failedData));
    yield* Effect.promise(() => cloud.githubDispatched(target.build, dispatchedRun(RUN)));
    // The Deployment's stop commits; then another writer holds the Environment, so ending its build gets no answer.
    const [stopped, held] = [yield* Deferred.make<void>(), yield* Deferred.make<void>()];
    const fail = cloud.runDeployment.bind(cloud);
    cloud.runDeployment = async (...args) => {
      const ran = await fail(...args);
      Effect.runSync(Deferred.succeed(stopped, undefined));
      await Effect.runPromise(Deferred.await(held));
      return ran;
    };
    const first = yield* provided(stopStoreDeploymentRun(ORGANIZATION, DEPLOYMENT, "worker-run")).pipe(Effect.exit, Effect.forkChild);
    yield* Deferred.await(stopped);
    const release = yield* holdEnvironment(provided);
    yield* Deferred.succeed(held, undefined);
    const stop = yield* Fiber.join(first);
    const unsettled = [...cancelled];
    yield* release;
    expect(unsettled).toEqual([]);
    expect(Exit.isFailure(stop)).toBe(true);
    yield* provided(stopStoreDeploymentRun(ORGANIZATION, DEPLOYMENT, "worker-run"));
    expect(cancelled).toEqual([RUN]);
  }),
  60_000,
);

it.live("a push's own run is never cancelled when its Deployment's run fails", () =>
  Effect.gen(function* () {
    const cancelled: number[] = [];
    const authority = fakeGithubAuthority({
      buildWorkflow: () => Effect.succeed({ ...activeWorkflow, template: "rule" }),
      buildRuns: () => Effect.succeed([{ id: RUN, url: runUrl(RUN), headSha: HEAD, event: "push", status: "in_progress", conclusion: null }]),
      cancelBuild: ({ runId }) => Effect.sync(() => void cancelled.push(runId)),
    });
    const { provided, web } = yield* pinnedWeb(authority);
    const cloud = yield* provided(cloudStore);
    cloud.githubStart = async () => ({ kind: "follow", workflow: ".github/workflows/ployz-build.yml", workflow_ref: WORKFLOW, commit: HEAD, foreign: [] });
    yield* provided(recordStoreDeploymentRun("worker-run", failedData));
    expect(yield* provided(startStoreGithubBuild(ORGANIZATION, target))).toEqual({ kind: "dispatched", runId: RUN });
    yield* provided(stopStoreDeploymentRun(ORGANIZATION, DEPLOYMENT, "worker-run"));
    expect(yield* web()).toMatchObject({ status: "failed" });
    expect(cancelled).toEqual([]);
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

/** GitHub as Cloud's dispatches see it: each dispatch starts run 9001, and each cancel is kept. */
const dispatchingAuthority = (cancelled: number[]) => fakeGithubAuthority({
  buildWorkflow: () => Effect.succeed(activeWorkflow),
  dispatchBuild: () => Effect.succeed({ runId: RUN, runUrl: runUrl(RUN), headSha: HEAD, workflowRef: WORKFLOW }),
  cancelBuild: ({ runId }) => Effect.sync(() => void cancelled.push(runId)),
});

const deploymentStatus = (cloud: ConfigStore) =>
  Effect.promise(() => cloud.read(ORGANIZATION, { query: "deployment", id: DEPLOYMENT })).pipe(Effect.map((view) => view.status));

it.live("an older run's stop leaves the run a newer run of the same Deployment dispatched, and the Deployment to it", () =>
  Effect.gen(function* () {
    const cancelled: number[] = [];
    const { provided, web } = yield* pinnedWeb(dispatchingAuthority(cancelled));
    // Run A was handed the Deployment and stopped before it claimed it; run B, handed it again, dispatched 9001.
    yield* provided(recordStoreDeploymentRun("worker-old", failedData));
    yield* provided(recordStoreDeploymentRun("worker-run", failedData));
    expect(yield* provided(startStoreGithubBuild(ORGANIZATION, target))).toEqual({ kind: "dispatched", runId: RUN });
    yield* provided(stopStoreDeploymentRun(ORGANIZATION, DEPLOYMENT, "worker-old"));
    expect(cancelled).toEqual([]);
    expect(yield* web()).toMatchObject({ status: "building" });
    expect(yield* deploymentStatus(yield* provided(cloudStore))).toBe("queued");
    // B's own stop still ends its build and cancels its run.
    yield* provided(stopStoreDeploymentRun(ORGANIZATION, DEPLOYMENT, "worker-run"));
    expect(cancelled).toEqual([RUN]);
  }),
  60_000,
);

it.live("a Deployment whose runner's lease lapsed keeps its run while that runner may still renew it", () =>
  Effect.gen(function* () {
    const cancelled: number[] = [];
    const { provided, web } = yield* pinnedWeb(dispatchingAuthority(cancelled));
    const cloud = yield* provided(cloudStore);
    expect(yield* provided(startStoreGithubBuild(ORGANIZATION, target))).toEqual({ kind: "dispatched", runId: RUN });
    yield* Effect.promise(() => cloud.claimDeployment(ORGANIZATION, DEPLOYMENT, ATTEMPT));
    // The lease reads lapsed, so the Deployment shows unknown, while its runner's renewal may still commit.
    const { drizzle } = yield* provided(Database);
    yield* provided(drizzle.execute(sql`update config_deployment set lease = 1 where id = ${DEPLOYMENT}`));
    expect(yield* deploymentStatus(cloud)).toBe("unknown");
    // A user's cancel of another Deployment, or any stop that is not its runner's, decides nothing from that view.
    const stop = yield* provided(cancelStoreGithubBuilds(DEPLOYMENT, null)).pipe(Effect.exit);
    expect(Exit.isFailure(stop)).toBe(true);
    expect(cancelled).toEqual([]);
    expect(yield* web()).toMatchObject({ status: "building" });
  }),
  60_000,
);

it.live("a stop whose build's end commits and whose answer is lost still cancels the run on its next try", () =>
  Effect.gen(function* () {
    const cancelled: number[] = [];
    const { provided } = yield* pinnedWeb(dispatchingAuthority(cancelled));
    const cloud = yield* provided(cloudStore);
    yield* provided(recordStoreDeploymentRun("worker-run", failedData));
    expect(yield* provided(startStoreGithubBuild(ORGANIZATION, target))).toEqual({ kind: "dispatched", runId: RUN });
    // The write that ends the build commits; its answer never reaches Cloud.
    const stopBuilds = cloud.githubStop.bind(cloud);
    cloud.githubStop = async (...args) => {
      await stopBuilds(...args);
      cloud.githubStop = stopBuilds;
      throw unavailable();
    };
    const first = yield* provided(stopStoreDeploymentRun(ORGANIZATION, DEPLOYMENT, "worker-run")).pipe(Effect.exit);
    expect(Exit.isFailure(first)).toBe(true);
    expect(cancelled).toEqual([]);
    yield* provided(stopStoreDeploymentRun(ORGANIZATION, DEPLOYMENT, "worker-run"));
    expect(cancelled).toEqual([RUN]);
  }),
  60_000,
);

it.live("a run recorded before Cloud marked its own is never cancelled: it may be a push's", () =>
  Effect.gen(function* () {
    const cancelled: number[] = [];
    const { provided, web } = yield* pinnedWeb(dispatchingAuthority(cancelled));
    const cloud = yield* provided(cloudStore);
    yield* provided(recordStoreDeploymentRun("worker-run", failedData));
    yield* Effect.promise(() => cloud.githubDispatched(target.build, {
      run_id: RUN, run_url: runUrl(RUN), workflow_ref: WORKFLOW, repository: "acme/web", installation_id: 7,
    }));
    yield* provided(stopStoreDeploymentRun(ORGANIZATION, DEPLOYMENT, "worker-run"));
    expect(yield* web()).toMatchObject({ status: "failed" });
    expect(cancelled).toEqual([]);
  }),
  60_000,
);

it.live("a waiting build pinned before Cloud reserved its asks fails with why instead of being asked for again", () =>
  Effect.gen(function* () {
    const cancelled: number[] = [];
    const authority = dispatchingAuthority(cancelled);
    const { provided, web } = yield* pinnedWeb(authority);
    const { drizzle } = yield* provided(Database);
    yield* provided(drizzle.execute(sql`update config_build set reservable = 0 where deployment_id = ${DEPLOYMENT}`));
    expect(yield* provided(startStoreGithubBuild(ORGANIZATION, target))).toEqual({ kind: "done" });
    expect(authority.calls.filter((call) => call.operation === "dispatchBuild")).toEqual([]);
    expect(yield* web()).toMatchObject({ status: "failed", message: "Cloud restarted while it may have been starting this build on GitHub. Push again to build it." });
  }),
  60_000,
);

it.live("a run that ended with its stop's hand-off lost is stopped by the sweep, and its build never waits on GitHub", () =>
  Effect.gen(function* () {
    const cancelled: number[] = [];
    const { provided, web } = yield* pinnedWeb(dispatchingAuthority(cancelled));
    const cloud = yield* provided(cloudStore);
    const runEffect: EffectRunner<StoreServices> = (program) => Effect.runPromise(provided(program));
    yield* provided(recordStoreDeploymentRun("worker-run", failedData));
    // The run reserved its build and ended before GitHub answered; its stop got no answer, and handing it on failed.
    expect(yield* Effect.promise(() => cloud.githubAsk(target.build, ATTEMPT))).toBe("reserved");
    const release = yield* holdEnvironment(provided);
    const steps = failureSteps();
    steps.step.sendEvent = async () => { throw new Error("Inngest is unavailable"); };
    const worker = createRunStoreDeployment(new Inngest({ id: "sweep-test" }), runEffect);
    yield* Effect.exit(Effect.tryPromise(() => invokeOnFailure(worker, "worker-run", { name: configDeploymentAdmitted.name, data: failedData }, steps)));
    yield* release;
    expect(yield* web()).toMatchObject({ status: "pending", message: "Asking GitHub for a run" });
    // Later, Inngest reports the run failed: the sweep hands its stop on again, and the stop settles it.
    const later = new Date(Date.now() + 16 * 60_000);
    const stale = yield* provided(staleStoreDeploymentRuns(() => Effect.succeed("failed" as const), later));
    expect(stale).toMatchObject([{ runId: "worker-run", deploymentId: DEPLOYMENT }]);
    const [found] = stale;
    if (found === undefined) return;
    const { result } = yield* Effect.promise(() => new InngestTestEngine({
      function: createStopStoreDeployment(new Inngest({ id: "sweep-test" }), runEffect),
      events: [configDeploymentStopRequested.create(found)],
      steps: [{ id: "wait-0", handler: () => null }],
    }).execute());
    expect(result).toEqual({ failed: DEPLOYMENT });
    expect(yield* web()).toMatchObject({ status: "pending", message: "Cloud stopped before GitHub answered" });
    expect(yield* provided(staleStoreDeploymentRuns(() => Effect.succeed("failed" as const), later))).toEqual([]);
    expect(cancelled).toEqual([]);
  }),
  60_000,
);
