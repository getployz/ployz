import crypto from "node:crypto";
import { it } from "@effect/vitest";
import type { ConfigCommand } from "@ployz/sdk";
import { sql } from "drizzle-orm";
import { Effect } from "effect";
import type { StoreCall } from "./store.contract";
import { expect, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { callStore, gatherTrusted } from "#/modules/config-store/config-store.server";
import { permissionsOf } from "#/modules/organization/permissions";
import {
  checkInRuleBuild, checkInStoreGithubBuild, checkStoreGithubBuild, grantRuleBuild, planStoreGithubBuilds, recordStoreGithubBuildSteps, startStoreGithubBuild,
  storeGithubBuildOpen,
} from "#/modules/config-store/store-github-builds.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { GithubOidcKeys } from "#/modules/github/github-oidc.server";
import { unlinkGithubInstallation } from "#/modules/github/github.repository";
import { type ConnectedRuntimeClient, OrganizationRuntime, type OrganizationRuntimeService } from "#/modules/runtime/organization-runtime.server";
import { RpcError } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { fakeGithubApi, fakeGithubAuthority } from "#/test/fake-github";
import type { GithubBuildWorkflowFile, GithubDispatchedBuild } from "#/modules/github/github-authority.server";
import { machineId } from "#/test/ids";
import { logged } from "#/test/logged";
import {
  acmeWeb, enrollStoreServer, recordStoreSealingKey, seedStoreGitService, seedStoreOrganization, storeTestCloud,
} from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000b001";
const PROJECT = "00000000-0000-4000-8000-00000000b002";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000b003";
const SERVICE = "00000000-0000-4000-8000-00000000b004";
const DEPLOYMENT = "00000000-0000-4000-8000-00000000b101";
const BUILD = `${DEPLOYMENT}.web`;
const HEAD = "c".repeat(40);
const RUN = 9001;
const WORKFLOW = "acme/web/.github/workflows/ployz-build.yml@refs/heads/main";
const here = { project: null, environment: null };
const ISSUER = "https://token.actions.githubusercontent.com";

const signing = crypto.generateKeyPairSync("rsa", { modulusLength: 2048 });
const jwk = { ...signing.publicKey.export({ format: "jwk" }), kid: "key-1" };
type RunnerClaims = { repository_id: string; job_workflow_ref: string; run_id: string; event_name: string };
/** A GitHub Actions OIDC token for the dispatched run, with `claims` overriding any claim. */
function oidcToken(claims: Partial<RunnerClaims> = {}, key: crypto.KeyObject = signing.privateKey) {
  const part = (json: string) => Buffer.from(json).toString("base64url");
  const body = `${part(JSON.stringify({ alg: "RS256", kid: "key-1" }))}.${part(JSON.stringify({
    iss: ISSUER, aud: "http://localhost:3000", exp: Math.floor(Date.now() / 1000) + 300,
    repository_id: "42", job_workflow_ref: WORKFLOW, run_id: String(RUN), event_name: "workflow_dispatch", ...claims,
  }))}`;
  return `${body}.${crypto.sign("RSA-SHA256", Buffer.from(body), key).toString("base64url")}`;
}

const activeWorkflow: GithubBuildWorkflowFile = { fullName: "acme/web", defaultBranch: "main", state: "active", canonical: true, dispatchable: true };
const noWorkflow: GithubBuildWorkflowFile = { fullName: "acme/web", defaultBranch: "main", state: null, canonical: false, dispatchable: false };
const dispatchedRun = (runId: number): GithubDispatchedBuild =>
  ({ runId, runUrl: `https://github.com/acme/web/actions/runs/${runId}`, headSha: HEAD, workflowRef: WORKFLOW });

/**
 * GitHub with `acme/web` (42) through installation 7: its build workflow as `workflow` says on each look, and each
 * dispatch starting the run `dispatch` names. Every run is in progress and takes a cancel.
 */
function acmeGithub(answers: { workflow: () => GithubBuildWorkflowFile; dispatch?: () => number }) {
  const { dispatch } = answers;
  const api = fakeGithubApi({ "/repos/acme/web": { id: 42, full_name: "acme/web" } });
  const authority = fakeGithubAuthority({
    buildWorkflow: () => Effect.sync(answers.workflow),
    dispatchBuild: () => dispatch === undefined ? Effect.die("no dispatch in this test") : Effect.sync(() => dispatchedRun(dispatch())),
    buildRun: ({ runId }) => Effect.succeed({ id: runId, url: dispatchedRun(runId).runUrl, headSha: HEAD, event: "workflow_dispatch", status: "in_progress", conclusion: null }),
    cancelBuild: () => Effect.void,
  });
  const cancelsOf = (runId: number) => authority.calls.filter((call) => call.operation === "cancelBuild" && call.input.runId === runId).length;
  return { cancelsOf, services: { github: api.service, authority: authority.service } };
}

/** `acme/web`, whose build workflow is missing, then edited away from Ployz's, then Ployz's own, as `workflowNow` says. */
let workflowNow: "missing" | "edited" | "ready" = "missing";
const github = acmeGithub({
  workflow: () => workflowNow === "ready" ? activeWorkflow : workflowNow === "edited" ? { ...activeWorkflow, canonical: false, dispatchable: false } : noWorkflow,
});

it.live(
  "a Store build goes to GitHub first; its check-in must come from the dispatched repository, workflow and run; a cancel ends it",
  () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud(github.services);
      const provided = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(
        Effect.provide(services),
        Effect.provideService(GithubOidcKeys, { keys: Effect.succeed([jwk]) }),
      );
      yield* provided(seedStoreOrganization(ORGANIZATION));
      const store = yield* provided(seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }));
      const write = (command: ConfigCommand) => Effect.promise(() => store.write(ORGANIZATION, command, acmeWeb()));
      yield* write({ command: "admit", admit: "deploy", id: DEPLOYMENT, environment: here, services: [], version: null, accept_volume_loss: [] });
      const view = () => Effect.promise(() => store.read(ORGANIZATION, { query: "deployment", id: DEPLOYMENT }));

      // Auto tries GitHub first: GitHub gets the pinned build.
      yield* Effect.promise(() => store.pinSources(DEPLOYMENT, { web: HEAD }));
      const data = { organizationId: ORGANIZATION, environmentId: ENVIRONMENT, deploymentId: DEPLOYMENT, trigger: "user" as const };
      const [target] = yield* provided(planStoreGithubBuilds(data));
      if (target === undefined) return expect.unreachable("GitHub gets the build");
      expect(target).toEqual({
        build: BUILD, service: "web", repository: "acme/web", repositoryId: 42, installationId: 7, hasNext: true,
      });

      // Without the build workflow GitHub is skipped at once, with why, and the servers take it.
      expect(yield* provided(startStoreGithubBuild(ORGANIZATION, target))).toEqual({ kind: "done" });
      expect(yield* view()).toMatchObject({
        builds: [{ service: "web", commit: HEAD, status: "pending", message: "acme/web has no .github/workflows/ployz-build.yml on its default branch" }],
      });
      // A skipped build is no longer GitHub's to plan.
      expect(yield* provided(planStoreGithubBuilds(data))).toEqual([]);


      // A build GitHub holds: only its dispatched run's token checks in.
      yield* write({ command: "admit", admit: "deploy", id: "00000000-0000-4000-8000-00000000b102", environment: here, services: [], version: null, accept_volume_loss: [] });
      const next = `00000000-0000-4000-8000-00000000b102.web`;
      yield* Effect.promise(() => store.pinSources("00000000-0000-4000-8000-00000000b102", { web: HEAD }));
      yield* Effect.promise(() => store.githubDispatched(next, {
        run_id: RUN, run_url: "https://github.com/acme/web/actions/runs/9001", workflow_ref: WORKFLOW, repository: "acme/web", installation_id: 7,
      }));
      const request = (token: string) =>
        new Request(`http://localhost:3000/api/builds/${next}/check-in`, { method: "POST", headers: { authorization: `Bearer ${token}` } });
      const rejection = (token: string) => provided(Effect.flip(checkInStoreGithubBuild(request(token), next, "")));
      expect(yield* rejection(oidcToken({}, crypto.generateKeyPairSync("rsa", { modulusLength: 2048 }).privateKey))).toMatchObject({ _tag: "Unauthorized" });
      expect(yield* rejection(oidcToken({ repository_id: "43" }))).toMatchObject({ _tag: "Forbidden", message: "The token is for another repository" });
      expect(yield* rejection(oidcToken({ job_workflow_ref: WORKFLOW.replace("main", "evil") })))
        .toMatchObject({ _tag: "Forbidden", message: "The token is for another workflow or branch" });
      expect(yield* rejection(oidcToken({ run_id: "9002" }))).toMatchObject({ _tag: "Forbidden", message: "The token is for another run" });
      expect(yield* rejection(oidcToken({ event_name: "push" }))).toMatchObject({ _tag: "Forbidden", message: "The run was not dispatched by Ployz" });
      // The right run with no Server to mint a grant: the runner retries its check-in.
      expect(yield* rejection(oidcToken())).toMatchObject({ _tag: "BuildGrantUnavailable" });
      const holding = (body: string) => provided(Effect.flip(checkInStoreGithubBuild(new Request(
        `http://localhost:3000/api/builds/${next}/check-in`,
        { method: "POST", headers: { authorization: `Bearer ${oidcToken()}`, "content-type": "application/json" } },
      ), next, body)));
      expect(yield* holding(JSON.stringify({ holder: "d".repeat(64) }))).toMatchObject({ _tag: "BuildGrantUnavailable" });
      expect(yield* holding(JSON.stringify({}))).toMatchObject({ _tag: "BuildGrantUnavailable" });
      for (const body of [JSON.stringify({ holder: "D".repeat(64) }), JSON.stringify({ holder: "d".repeat(63) }), "[]", "not json"]) {
        expect(yield* holding(body)).toMatchObject({ _tag: "Validation" });
      }
      // A rule build's check-in names its commit; a build Cloud dispatched has no rule to grant under.
      for (const body of ["", "{}", JSON.stringify({ commit: HEAD.toUpperCase() })]) {
        expect(yield* provided(Effect.flip(checkInRuleBuild(request(oidcToken({ event_name: "push" })), body))))
          .toMatchObject({ _tag: "Validation", message: "The check-in is not a commit." });
      }
      const ruleGrant = yield* provided(Effect.flip(grantRuleBuild(request(oidcToken()), next, "{}")));
      expect(ruleGrant).toMatchObject({ _tag: "Validation" });
      // Build Steps before the check-in are refused.
      const steps = yield* provided(Effect.flip(recordStoreGithubBuildSteps(request(oidcToken()), next, JSON.stringify({ from: 0, events: [] }))));
      expect(steps).toMatchObject({ _tag: "Conflict", message: "This build has not checked in" });

      // Cancelling the Deployment ends the build GitHub holds; a late check-in is refused.
      const cancelledBefore = github.cancelsOf(9001);
      const cancelled = yield* provided(callStore(ORGANIZATION, "00000000-0000-4000-8000-00000000b0ff", {
        operation: "write", command: { command: "cancel", deployment: "00000000-0000-4000-8000-00000000b102" },
      }, { source: "dashboard" }, { approval: "not_required", permissions: permissionsOf("owner") }));
      expect(cancelled).toMatchObject({ ok: true });
      expect(github.cancelsOf(9001)).toBeGreaterThan(cancelledBefore);
      expect(yield* Effect.promise(() => store.githubBuild(next))).toMatchObject({ held: { status: "failed" } });
      expect(yield* rejection(oidcToken())).toMatchObject({ _tag: "Conflict" });

      // A walk that starts with the Servers leaves the build to them, unless no Server takes builds: then GitHub.
      yield* write({ command: "set_build_order", build_order: "servers-then-github" });
      const serversFirst = "00000000-0000-4000-8000-00000000b103";
      yield* write({ command: "admit", admit: "deploy", id: serversFirst, environment: here, services: [], version: null, accept_volume_loss: [] });
      yield* Effect.promise(() => store.pinSources(serversFirst, { web: HEAD }));
      const plan = { ...data, deploymentId: serversFirst };
      expect(yield* provided(planStoreGithubBuilds(plan))).toEqual([]);
      const noBuilders = asTestDouble<OrganizationRuntimeService>()({
        cancel: () => Effect.void,
        open: () => Effect.succeed({ status: "connected" as const, connected: asTestDouble<ConnectedRuntimeClient>()({
          watchFirstFrame: () => Effect.succeed({ machines: [{ machine: { accepts_builds: false } }] }),
        }) }),
      });
      expect(yield* provided(planStoreGithubBuilds(plan).pipe(Effect.provideService(OrganizationRuntime, noBuilders))))
        .toMatchObject([{ build: `${serversFirst}.web`, hasNext: false }]);

      // Cloud dispatches no workflow file but the one Ployz offers: GitHub would run what the file says.
      workflowNow = "edited";
      const edited = "00000000-0000-4000-8000-00000000b1e0";
      yield* write({ command: "admit", admit: "deploy", id: edited, environment: here, services: [], version: null, accept_volume_loss: [] });
      yield* Effect.promise(() => store.pinSources(edited, { web: HEAD }));
      // The Store would hand GitHub the build to dispatch; Cloud refuses to dispatch that file.
      vi.spyOn(yield* provided(cloudStore), "githubStart").mockResolvedValueOnce({ kind: "dispatch", runner: "ubuntu-latest", foreign: [] });
      expect(yield* provided(startStoreGithubBuild(ORGANIZATION, { ...target, build: `${edited}.web` }))).toEqual({ kind: "done" });
      expect(yield* Effect.promise(() => store.read(ORGANIZATION, { query: "deployment", id: edited }))).toMatchObject({
        builds: [{ service: "web", status: "pending", message: "acme/web's .github/workflows/ployz-build.yml isn't the one Ployz offers" }],
      });

      // A Server whose record says it can't read a Build Grant back sends the build to its next Builder, by name.
      workflowNow = "ready";
      const oldServer = "00000000-0000-4000-8000-00000000b104";
      yield* write({ command: "admit", admit: "deploy", id: oldServer, environment: here, services: [], version: null, accept_volume_loss: [] });
      yield* Effect.promise(() => store.pinSources(oldServer, { web: HEAD }));
      const lacking = asTestDouble<OrganizationRuntimeService>()({
        cancel: () => Effect.void,
        open: () => Effect.succeed({ status: "connected" as const, connected: asTestDouble<ConnectedRuntimeClient>()({
          watchFirstFrame: () => Effect.succeed({ machines: [
            { machine: { name: "fsn-1", runtime: { capabilities: [] } } },
            { machine: { name: "hel-1", runtime: { capabilities: ["ployz.logs.tail.v1"] } } },
          ] }),
        }) }),
      });
      const oldTarget = { ...target, build: `${oldServer}.web` };
      expect(yield* provided(startStoreGithubBuild(ORGANIZATION, oldTarget).pipe(Effect.provideService(OrganizationRuntime, lacking))))
        .toEqual({ kind: "done" });
      expect(yield* Effect.promise(() => store.read(ORGANIZATION, { query: "deployment", id: oldServer }))).toMatchObject({
        builds: [{ service: "web", status: "pending", message: "GitHub builds need a newer Ployz on hel-1" }],
      });

      // An admission carries how many Servers could run it: none here.
      const admission = { operation: "write", command: {
        command: "admit", admit: "deploy", id: crypto.randomUUID(), environment: { project: null, environment: null },
        services: [], version: null, accept_volume_loss: [],
      } } satisfies StoreCall;
      const trusted = yield* provided(gatherTrusted(ORGANIZATION, admission, (query) => store.read(ORGANIZATION, query)));
      expect(trusted.servers).toBe(0);

      const disconnected = "00000000-0000-4000-8000-00000000b105";
      yield* write({ command: "admit", admit: "deploy", id: disconnected, environment: here, services: [], version: null, accept_volume_loss: [] });
      const held = `${disconnected}.web`;
      yield* Effect.promise(() => store.pinSources(disconnected, { web: HEAD }));
      yield* Effect.promise(() => store.githubDispatched(held, {
        run_id: 9105, run_url: "https://github.com/acme/web/actions/runs/9105", workflow_ref: WORKFLOW, repository: "acme/web", installation_id: 7,
      }));
      expect(yield* provided(unlinkGithubInstallation({ organizationId: ORGANIZATION, installationId: 7 }))).toBe(true);
      expect(yield* provided(Effect.flip(checkInStoreGithubBuild(request(oidcToken({ run_id: "9105" })), held, ""))))
        .toMatchObject({ _tag: "Forbidden", message: "GitHub account `acme` was disconnected" });
      expect(yield* provided(Effect.flip(recordStoreGithubBuildSteps(request(oidcToken({ run_id: "9105" })), held, JSON.stringify({ from: 0, events: [] })))))
        .toMatchObject({ _tag: "Forbidden", message: "GitHub account `acme` was disconnected" });
      expect(yield* Effect.promise(() => store.githubBuild(held))).toMatchObject({ held: { status: "building" } });
      const look = yield* provided(checkStoreGithubBuild(ORGANIZATION, { ...target, build: held }, { ended: false, startLimit: false }));
      expect(look).toEqual({ failed: "GitHub account `acme` was disconnected" });
      expect(yield* Effect.promise(() => store.githubBuild(held))).toMatchObject({ held: { status: "failed" } });
    }),
  60_000,
);

it.live("a build that checked in reports its steps until its Organization disconnects GitHub, then is refused", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud(github.services);
    const provided = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(
      Effect.provide(services),
      Effect.provideService(GithubOidcKeys, { keys: Effect.succeed([jwk]) }),
    );
    // The runner checked in before the disconnect, so the Store takes its reports.
    const githubReport = vi.spyOn(yield* provided(cloudStore), "githubReport").mockResolvedValue({ received: 1, ended: false });
    yield* provided(seedStoreOrganization(ORGANIZATION));
    const store = yield* provided(seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }));
    yield* Effect.promise(() => store.write(ORGANIZATION, {
      command: "admit", admit: "deploy", id: DEPLOYMENT, environment: here, services: [], version: null, accept_volume_loss: [],
    }, acmeWeb()));
    yield* Effect.promise(() => store.pinSources(DEPLOYMENT, { web: HEAD }));
    yield* Effect.promise(() => store.githubDispatched(BUILD, {
      run_id: RUN, run_url: `https://github.com/acme/web/actions/runs/${RUN}`, workflow_ref: WORKFLOW, repository: "acme/web", installation_id: 7,
    }));
    const steps = () => recordStoreGithubBuildSteps(
      new Request(`http://localhost:3000/api/builds/${BUILD}/steps`, { method: "POST", headers: { authorization: `Bearer ${oidcToken()}` } }),
      BUILD, JSON.stringify({ from: 0, events: [] }),
    );

    expect(yield* provided(steps())).toEqual({ received: 1 });
    expect(yield* provided(unlinkGithubInstallation({ organizationId: ORGANIZATION, installationId: 7 }))).toBe(true);
    expect(yield* provided(Effect.flip(steps()))).toMatchObject({ _tag: "Forbidden", message: "GitHub account `acme` was disconnected" });
    expect(githubReport).toHaveBeenCalledTimes(1);
  }), 60_000);

it.live("a Store build's start, check-in and finish reach a Server of the pairing's Cluster once its Sealing Key is known, as a Deploy does, and log one they left", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud(github.services);
    const provided = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(
      Effect.provide(services),
      Effect.provideService(GithubOidcKeys, { keys: Effect.succeed([jwk]) }),
    );
    const cloud = yield* provided(cloudStore);
    const githubStart = vi.spyOn(cloud, "githubStart").mockResolvedValue({ kind: "reused", foreign: [] });
    const checkIn = {
      grant: "ployzgrant1:fake", commit: HEAD, fingerprint: "fake", ployzVersion: "0.0.0", deployment: { namespace: "shop-production", snapshots: [] },
    };
    const githubCheckIn = vi.spyOn(cloud, "githubCheckIn").mockResolvedValue({ ...checkIn, foreign: [machineId("9")] });
    vi.spyOn(cloud, "githubReport").mockResolvedValue({ received: 1, ended: true });
    const githubFinish = vi.spyOn(cloud, "githubFinish").mockResolvedValue({ ended: "built" });
    yield* provided(Effect.all([seedStoreOrganization(ORGANIZATION), enrollStoreServer(ORGANIZATION)]));
    const store = yield* provided(seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }));
    yield* Effect.promise(() => store.write(ORGANIZATION, {
      command: "admit", admit: "deploy", id: DEPLOYMENT, environment: here, services: [], version: null, accept_volume_loss: [],
    }, acmeWeb()));
    yield* Effect.promise(() => store.pinSources(DEPLOYMENT, { web: HEAD }));
    const servers = [{ machine_id: machineId("0"), management: `ployz1:cloud:${machineId("0")}` }];
    const target = { build: BUILD, service: "web", repository: "acme/web", repositoryId: 42, installationId: null, hasNext: true };
    const runner = (step: string) => new Request(`http://localhost:3000/api/builds/${BUILD}/${step}`, {
      method: "POST", headers: { authorization: `Bearer ${oidcToken()}` },
    });

    // No Sealing Key recorded yet: any of the pairing's Servers, as before.
    expect(yield* provided(startStoreGithubBuild(ORGANIZATION, target))).toEqual({ kind: "done" });
    expect(githubStart).toHaveBeenLastCalledWith(BUILD, servers, null);

    const pairingKey = `${"P".repeat(43)}=`;
    const expectation = { sealingPublicKey: pairingKey };
    yield* provided(recordStoreSealingKey(ORGANIZATION, pairingKey));
    githubStart.mockResolvedValueOnce({ kind: "reused", foreign: [machineId("9")] });
    using errors = logged("ERROR", "in another Cluster", machineId("9"));
    yield* provided(startStoreGithubBuild(ORGANIZATION, target));
    expect(githubStart).toHaveBeenLastCalledWith(BUILD, servers, expectation);
    expect(errors.count()).toBe(1);
    // GitHub holds the build from here: its runner checks in, reports and is finished.
    yield* Effect.promise(() => store.githubDispatched(BUILD, {
      run_id: RUN, run_url: `https://github.com/acme/web/actions/runs/${RUN}`, workflow_ref: WORKFLOW, repository: "acme/web", installation_id: 7,
    }));
    // The runner receives only what it pushes and builds with.
    expect(yield* provided(checkInStoreGithubBuild(runner("check-in"), BUILD, ""))).toStrictEqual(checkIn);
    expect(githubCheckIn).toHaveBeenCalledExactlyOnceWith(BUILD, expect.anything(), null, servers, expectation);
    expect(errors.count()).toBe(2);
    expect(yield* provided(recordStoreGithubBuildSteps(runner("steps"), BUILD, JSON.stringify({ from: 0, events: [] }))))
      .toEqual({ received: 1 });
    expect(githubFinish).toHaveBeenLastCalledWith(BUILD, "ended", servers, expectation);
    expect(yield* provided(checkStoreGithubBuild(ORGANIZATION, { ...target, installationId: 7 }, { ended: true, startLimit: false })))
      .toBe("done");
    expect(githubFinish).toHaveBeenCalledTimes(2);
    expect(githubFinish).toHaveBeenLastCalledWith(BUILD, "ended", servers, expectation);
  }), 60_000);

it.live("a build GitHub passed on before its run checked in is no longer GitHub's, even once the next Builder claims it", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud(github.services);
    const provided = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(
      Effect.provide(services),
      Effect.provideService(GithubOidcKeys, { keys: Effect.succeed([jwk]) }),
    );
    yield* provided(seedStoreOrganization(ORGANIZATION));
    const store = yield* provided(seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }));
    yield* Effect.promise(() => store.write(ORGANIZATION, {
      command: "admit", admit: "deploy", id: DEPLOYMENT, environment: here, services: [], version: null, accept_volume_loss: [],
    }, acmeWeb()));
    yield* Effect.promise(() => store.pinSources(DEPLOYMENT, { web: HEAD }));
    yield* Effect.promise(() => store.githubDispatched(BUILD, {
      run_id: RUN, run_url: `https://github.com/acme/web/actions/runs/${RUN}`, workflow_ref: WORKFLOW, repository: "acme/web", installation_id: 7,
    }));
    // The start limit passes it on before any grant, the same Skip a check-in records when only the next Builder can
    // build a sealed value. That one needs a Machine to answer the mint, which no test here runs.
    expect(yield* Effect.promise(() => store.githubFinish(BUILD, "start_limit", []))).toEqual({ ended: "pending" });
    expect(yield* Effect.promise(() => store.githubBuild(BUILD))).toEqual({ passed_on: { why: "no runner started the build in time" } });

    const target = {
      build: BUILD, service: "web", repository: "acme/web", repositoryId: 42, installationId: 7, hasNext: true,
    };
    const passedOn = { _tag: "Conflict", message: "GitHub passed this build to the next Builder." };
    const runner = () => new Request(`http://localhost:3000/api/builds/${BUILD}/check-in`, {
      method: "POST", headers: { authorization: `Bearer ${oidcToken()}` },
    });
    const late = Effect.gen(function* () {
      expect(yield* provided(Effect.flip(checkInStoreGithubBuild(runner(), BUILD, "")))).toMatchObject(passedOn);
      expect(yield* provided(Effect.flip(recordStoreGithubBuildSteps(runner(), BUILD, JSON.stringify({ from: 0, events: [] })))))
        .toMatchObject(passedOn);
      expect(yield* provided(storeGithubBuildOpen(target))).toBe(false);
      expect(yield* provided(checkStoreGithubBuild(ORGANIZATION, target, { ended: true, startLimit: false }))).toBe("done");
    });
    yield* late;

    // The next Builder takes it, with why; the run GitHub passed it on from still holds nothing.
    expect(yield* Effect.promise(() => store.claimDeployment(ORGANIZATION, DEPLOYMENT, "cloud-1"))).toMatchObject({ id: DEPLOYMENT });
    expect(yield* Effect.promise(() => store.read(ORGANIZATION, { query: "deployment", id: DEPLOYMENT }))).toMatchObject({
      builds: [{ service: "web", status: "pending", message: "no runner started the build in time" }],
    });
    yield* late;
  }), 60_000);

it.live("the start limit keeps a run whose check-in commits while it looks, and does not cancel it", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud(github.services);
    const provided = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(
      Effect.provide(services),
      Effect.provideService(GithubOidcKeys, { keys: Effect.succeed([jwk]) }),
    );
    yield* provided(seedStoreOrganization(ORGANIZATION));
    const store = yield* provided(seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }));
    yield* Effect.promise(() => store.write(ORGANIZATION, {
      command: "admit", admit: "deploy", id: DEPLOYMENT, environment: here, services: [], version: null, accept_volume_loss: [],
    }, acmeWeb()));
    yield* Effect.promise(() => store.pinSources(DEPLOYMENT, { web: HEAD }));
    yield* Effect.promise(() => store.githubDispatched(BUILD, {
      run_id: RUN, run_url: `https://github.com/acme/web/actions/runs/${RUN}`, workflow_ref: WORKFLOW, repository: "acme/web", installation_id: 7,
    }));
    const { drizzle } = yield* provided(Database);
    const grant = { id: "d".repeat(64), machine: machineId("0"), fingerprint: "f".repeat(64) };
    // The run's check-in as the Store's github_check_in commits it: a grant needs a Machine to mint it, which no test
    // here runs.
    const checkIn = drizzle.execute(sql`
      update config_build set github = jsonb_set(
        jsonb_set(github::jsonb, '{grant}', ${JSON.stringify(grant)}::jsonb), '{checked_in_at}', to_jsonb(${Math.floor(Date.now() / 1000)}::bigint)
      )::text where deployment_id = ${DEPLOYMENT} and service = 'web'`);
    const cloud = yield* provided(cloudStore);
    const finish = cloud.githubFinish.bind(cloud);
    // It commits after the start limit's look read the build with no grant, before githubFinish re-reads it.
    vi.spyOn(cloud, "githubFinish").mockImplementationOnce(async (...args) => {
      await Effect.runPromise(provided(checkIn));
      return finish(...args);
    });
    const target = { build: BUILD, service: "web", repository: "acme/web", repositoryId: 42, installationId: 7, hasNext: true };
    const cancelledBefore = github.cancelsOf(RUN);

    expect(yield* provided(checkStoreGithubBuild(ORGANIZATION, target, { ended: false, startLimit: true }))).toBe("waiting");
    expect(github.cancelsOf(RUN)).toBe(cancelledBefore);
    expect(yield* Effect.promise(() => store.githubBuild(BUILD))).toMatchObject({ held: { status: "building", grant } });
  }), 60_000);

it.live("a retried github-start whose first try handed the build to a run follows that run, and the start limit passes it on", () =>
  Effect.gen(function* () {
    let dispatches = 0;
    const retryGithub = acmeGithub({ workflow: () => activeWorkflow, dispatch: () => RUN + dispatches++ });
    const services = yield* storeTestCloud(retryGithub.services);
    const provided = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(
      Effect.provide(services),
      Effect.provideService(GithubOidcKeys, { keys: Effect.succeed([jwk]) }),
    );
    yield* provided(seedStoreOrganization(ORGANIZATION));
    const store = yield* provided(seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }));
    yield* Effect.promise(() => store.write(ORGANIZATION, {
      command: "admit", admit: "deploy", id: DEPLOYMENT, environment: here, services: [], version: null, accept_volume_loss: [],
    }, acmeWeb()));
    yield* Effect.promise(() => store.pinSources(DEPLOYMENT, { web: HEAD }));
    const cloud = yield* provided(cloudStore);
    vi.spyOn(cloud, "githubStart").mockResolvedValue({ kind: "dispatch", runner: "ubuntu-latest", foreign: [] });
    // The hand-off to run 9001 commits, and its answer is lost.
    const dispatched = cloud.githubDispatched.bind(cloud);
    vi.spyOn(cloud, "githubDispatched").mockImplementationOnce(async (...args) => {
      await dispatched(...args);
      throw new RpcError({ code: "unavailable", message: "The Config Store did not answer in time; the outcome is unknown", details: null });
    });
    const target = { build: BUILD, service: "web", repository: "acme/web", repositoryId: 42, installationId: 7, hasNext: true };
    const cancels = () => retryGithub.cancelsOf(RUN);

    expect(yield* provided(Effect.flip(startStoreGithubBuild(ORGANIZATION, target)))).toMatchObject({ code: "unavailable" });
    expect(cancels()).toBe(1);
    expect(yield* Effect.promise(() => store.githubBuild(BUILD))).toMatchObject({ held: { status: "building", run: { run_id: RUN } } });

    // Inngest runs the step again: it follows run 9001, and dispatches no other.
    expect(yield* provided(startStoreGithubBuild(ORGANIZATION, target))).toEqual({ kind: "dispatched", runId: RUN });
    expect(dispatches).toBe(1);
    expect(yield* provided(storeGithubBuildOpen(target))).toBe(true);
    // The run never checks in: the start limit passes it on, and the Servers are next.
    expect(yield* provided(checkStoreGithubBuild(ORGANIZATION, target, { ended: false, startLimit: true }))).toBe("done");
    expect(yield* Effect.promise(() => store.githubBuild(BUILD))).toEqual({ passed_on: { why: "no runner started the build in time" } });
    expect(yield* Effect.promise(() => store.read(ORGANIZATION, { query: "deployment", id: DEPLOYMENT }))).toMatchObject({
      builds: [{ service: "web", status: "pending", message: "no runner started the build in time" }],
    });
  }), 60_000);

it.live("a retry whose first hand-off commits after its own read follows that run, and cancels its own", () =>
  Effect.gen(function* () {
    let dispatches = 0;
    const retryGithub = acmeGithub({ workflow: () => activeWorkflow, dispatch: () => RUN + dispatches++ });
    const services = yield* storeTestCloud(retryGithub.services);
    const provided = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(
      Effect.provide(services),
      Effect.provideService(GithubOidcKeys, { keys: Effect.succeed([jwk]) }),
    );
    yield* provided(seedStoreOrganization(ORGANIZATION));
    const store = yield* provided(seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }));
    yield* Effect.promise(() => store.write(ORGANIZATION, {
      command: "admit", admit: "deploy", id: DEPLOYMENT, environment: here, services: [], version: null, accept_volume_loss: [],
    }, acmeWeb()));
    yield* Effect.promise(() => store.pinSources(DEPLOYMENT, { web: HEAD }));
    const cloud = yield* provided(cloudStore);
    vi.spyOn(cloud, "githubStart").mockResolvedValue({ kind: "dispatch", runner: "ubuntu-latest", foreign: [] });
    // The hand-off to run 9001 times out before it commits, and its work goes on: it commits once the retry has read
    // the build with no run, just before the retry hands the build to run 9002.
    const dispatched = cloud.githubDispatched.bind(cloud);
    let delayed: Parameters<typeof dispatched> | undefined;
    vi.spyOn(cloud, "githubDispatched").mockImplementation(async (...args) => {
      if (delayed === undefined) {
        delayed = args;
        throw new RpcError({ code: "unavailable", message: "The Config Store did not answer in time; the outcome is unknown", details: null });
      }
      await dispatched(...delayed);
      return dispatched(...args);
    });
    const target = { build: BUILD, service: "web", repository: "acme/web", repositoryId: 42, installationId: 7, hasNext: true };
    const { cancelsOf } = retryGithub;

    expect(yield* provided(Effect.flip(startStoreGithubBuild(ORGANIZATION, target)))).toMatchObject({ code: "unavailable" });
    yield* Effect.promise(() => expect(store.githubBuild(BUILD)).rejects.toMatchObject({ code: "not_found" }));

    // Inngest runs the step again. Run 9001's hand-off wins, so the retry cancels its own run and follows 9001.
    expect(yield* provided(startStoreGithubBuild(ORGANIZATION, target))).toEqual({ kind: "dispatched", runId: RUN });
    expect(dispatches).toBe(2);
    expect([cancelsOf(RUN), cancelsOf(RUN + 1)]).toEqual([1, 1]);
    expect(yield* Effect.promise(() => store.githubBuild(BUILD))).toMatchObject({ held: { status: "building", run: { run_id: RUN } } });
    // Run 9001 was cancelled and never checks in: the start limit passes it on, and the Servers are next.
    expect(yield* provided(checkStoreGithubBuild(ORGANIZATION, target, { ended: false, startLimit: true }))).toBe("done");
    expect(yield* Effect.promise(() => store.githubBuild(BUILD))).toEqual({ passed_on: { why: "no runner started the build in time" } });
  }), 60_000);

it.live("a retry that skips GitHub after its first hand-off commits late follows that run", () =>
  Effect.gen(function* () {
    let workflowChecks = 0;
    // The workflow is gone by the retry.
    const retryGithub = acmeGithub({ workflow: () => workflowChecks++ === 0 ? activeWorkflow : noWorkflow, dispatch: () => RUN });
    const services = yield* storeTestCloud(retryGithub.services);
    const provided = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(
      Effect.provide(services),
      Effect.provideService(GithubOidcKeys, { keys: Effect.succeed([jwk]) }),
    );
    yield* provided(seedStoreOrganization(ORGANIZATION));
    const store = yield* provided(seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }));
    yield* Effect.promise(() => store.write(ORGANIZATION, {
      command: "admit", admit: "deploy", id: DEPLOYMENT, environment: here, services: [], version: null, accept_volume_loss: [],
    }, acmeWeb()));
    yield* Effect.promise(() => store.pinSources(DEPLOYMENT, { web: HEAD }));
    const cloud = yield* provided(cloudStore);
    vi.spyOn(cloud, "githubStart").mockResolvedValue({ kind: "dispatch", runner: "ubuntu-latest", foreign: [] });
    // The hand-off to run 9001 times out before it commits, and commits once the retry has read the build with no run,
    // just before the retry records the skip.
    const dispatched = cloud.githubDispatched.bind(cloud);
    let delayed: Parameters<typeof dispatched> | undefined;
    vi.spyOn(cloud, "githubDispatched").mockImplementationOnce(async (...args) => {
      delayed = args;
      throw new RpcError({ code: "unavailable", message: "The Config Store did not answer in time; the outcome is unknown", details: null });
    });
    const skipped = cloud.githubSkip.bind(cloud);
    vi.spyOn(cloud, "githubSkip").mockImplementation(async (...args) => {
      if (delayed !== undefined) await dispatched(...delayed);
      return skipped(...args);
    });
    const target = { build: BUILD, service: "web", repository: "acme/web", repositoryId: 42, installationId: 7, hasNext: true };

    expect(yield* provided(Effect.flip(startStoreGithubBuild(ORGANIZATION, target)))).toMatchObject({ code: "unavailable" });
    // The skip is refused, since run 9001 holds the build: the retry follows it.
    expect(yield* provided(startStoreGithubBuild(ORGANIZATION, target))).toEqual({ kind: "dispatched", runId: RUN });
    expect(yield* Effect.promise(() => store.githubBuild(BUILD))).toMatchObject({ held: { status: "building", run: { run_id: RUN } } });
  }), 60_000);

it.live("a rule build follows its push, and an approved build dispatches, under the rule's own workflow file", () =>
  Effect.gen(function* () {
    // The rule's branch holds the rule's template, not the one the dashboard offers.
    const ruleTemplate = { ...activeWorkflow, canonical: false, dispatchable: false };
    const authority = fakeGithubAuthority({
      buildWorkflow: () => Effect.succeed(ruleTemplate),
      buildRuns: () => Effect.succeed([{ id: RUN, url: dispatchedRun(RUN).runUrl, headSha: HEAD, event: "push", status: "in_progress", conclusion: null }]),
      dispatchApprovedBuild: () => Effect.succeed({ runId: RUN + 1, runUrl: dispatchedRun(RUN + 1).runUrl }),
    });
    const services = yield* storeTestCloud({ github: fakeGithubApi().service, authority: authority.service });
    const provided = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(Effect.provide(services));
    yield* provided(seedStoreOrganization(ORGANIZATION));
    const store = yield* provided(seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }));
    const cloud = yield* provided(cloudStore);
    const started = vi.spyOn(cloud, "githubStart");
    const target = (deployment: string) =>
      ({ build: `${deployment}.web`, service: "web", repository: "acme/web", repositoryId: 42, installationId: 7, hasNext: true });
    const admitted = (deployment: string) => Effect.gen(function* () {
      yield* Effect.promise(() => store.write(ORGANIZATION, {
        command: "admit", admit: "deploy", id: deployment, environment: here, services: [], version: null, accept_volume_loss: [],
      }, acmeWeb()));
      yield* Effect.promise(() => store.pinSources(deployment, { web: HEAD }));
    });

    yield* admitted(DEPLOYMENT);
    started.mockResolvedValueOnce({ kind: "follow", workflow: ".github/workflows/ployz-build.yml", workflow_ref: WORKFLOW, commit: HEAD, foreign: [] });
    expect(yield* provided(startStoreGithubBuild(ORGANIZATION, target(DEPLOYMENT)))).toEqual({ kind: "dispatched", runId: RUN });

    const approved = "00000000-0000-4000-8000-00000000b1e1";
    yield* admitted(approved);
    started.mockResolvedValueOnce({ kind: "approved", workflow: ".github/workflows/ployz-build.yml", workflow_ref: WORKFLOW, branch: "main", commit: HEAD, foreign: [] });
    expect(yield* provided(startStoreGithubBuild(ORGANIZATION, target(approved)))).toEqual({ kind: "dispatched", runId: RUN + 1 });
    expect(authority.calls.filter((call) => call.operation === "dispatchApprovedBuild").map((call) => call.input))
      .toEqual([{ installationId: 7, repositoryId: 42, branch: "main", commit: HEAD }]);
    expect(authority.calls.some((call) => call.operation === "dispatchBuild")).toBe(false);
  }), 60_000);
