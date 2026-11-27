import crypto from "node:crypto";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Clock, Deferred, Effect, Exit, Fiber, Redacted } from "effect";
import { TestClock } from "effect/testing";
import { expect } from "vitest";
import { planStoreGithubBuilds, startStoreGithubBuild } from "#/modules/config-store/store-github-builds.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { GithubAuthority, type GithubBuildWorkflowFile, makeGithubAuthorityLocal } from "#/modules/github/github-authority.server";
import { Database } from "#/server/database.server";
import { fakeGithubApi, fakeGithubAuthority } from "#/test/fake-github";
import { acmeWeb, seedStoreGitService, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000b001";
const PROJECT = "00000000-0000-4000-8000-00000000b002";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000b003";
const SERVICE = "00000000-0000-4000-8000-00000000b004";
const DEPLOYMENT = "00000000-0000-4000-8000-00000000b101";
const HEAD = "c".repeat(40);
const RUN = 9001;
const WORKFLOW = "acme/web/.github/workflows/ployz-build.yml@refs/heads/main";
const here = { project: null, environment: null };
const target = { build: `${DEPLOYMENT}.web`, service: "web", repository: "acme/web", repositoryId: 42, installationId: 7, hasNext: true };
const activeWorkflow: GithubBuildWorkflowFile = { fullName: "acme/web", defaultBranch: "main", state: "active", template: "build", dispatchable: true };
const runUrl = (runId: number) => `https://github.com/acme/web/actions/runs/${runId}`;

/** Cloud with `acme/web`'s web Service admitted and pinned, whose GitHub dispatches through `authority`. */
const pinnedWeb = Effect.fn(function* (authority: ReturnType<typeof fakeGithubAuthority>, env?: Record<string, string>) {
  const api = fakeGithubApi({ "/repos/acme/web": { id: 42, full_name: "acme/web" } });
  const services = yield* storeTestCloud({ github: api.service, authority: authority.service, ...(env && { env }) });
  const provided = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(Effect.provide(services));
  yield* provided(seedStoreOrganization(ORGANIZATION));
  const store = yield* provided(seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }));
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "admit", admit: "deploy", id: DEPLOYMENT, environment: here, services: [], version: null, accept_volume_loss: [],
  }, acmeWeb()));
  yield* Effect.promise(() => store.pinSources(DEPLOYMENT, { web: HEAD }));
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

it.live("a run GitHub took while the Store can't record it is cancelled, so its retry leaves one run going", () =>
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
    // Another writer holds the Environment's row until the Store gives up on recording the run.
    const release = yield* holdEnvironment(provided);
    const first = yield* provided(startStoreGithubBuild(ORGANIZATION, target)).pipe(Effect.exit);
    yield* release;
    expect(Exit.isFailure(first)).toBe(true);
    expect(yield* web()).toMatchObject({ status: "pending", message: null });

    expect(yield* provided(startStoreGithubBuild(ORGANIZATION, target))).toEqual({ kind: "dispatched", runId: RUN + 1 });
    const cancelled = authority.calls.flatMap((call) => call.operation === "cancelBuild" ? [call.input.runId] : []);
    expect(cancelled).toEqual([RUN]);
    expect(dispatched.filter((runId) => !cancelled.includes(runId))).toEqual([RUN + 1]);
  }), 60_000);

it.effect.each([false, true])(
  "a dispatch whose answer stalls ends inside its bound, and the build goes to the next Builder (Environment locked: %s)",
  (locked) => Effect.gen(function* () {
    const dispatched: number[] = [];
    const taken = yield* Deferred.make<void>();
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
  }),
  60_000,
);
