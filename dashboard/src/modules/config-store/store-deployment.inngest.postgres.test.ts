import { gzipSync } from "node:zlib";
import { it } from "@effect/vitest";
import { InngestTestEngine } from "@inngest/test";
import type { ConfigCommand, ConfigTrusted } from "@ployz/sdk";
import { Effect, Layer, Schema } from "effect";
import { Inngest } from "inngest";
import { Header } from "tar";
import { expect } from "vitest";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { createCancelStoreDeployment, createRunStoreDeployment } from "#/modules/config-store/store-deployment.inngest";
import { recordStoreDeploymentRun, unclaimedStoreDeployments } from "#/modules/config-store/store-deployment.server";
import { GithubObservationError, type GithubApiService } from "#/modules/github/github-observation.api";
import { configDeploymentAdmittedEvent } from "#/modules/inngest/events";
import { makeInngestEffectRunner } from "#/server/run.server";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000a001";
const PROJECT = "00000000-0000-4000-8000-00000000a002";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000a003";
const SERVICE = "00000000-0000-4000-8000-00000000a004";
const DEPLOYED = "00000000-0000-4000-8000-00000000a101";
const CANCELLED = "00000000-0000-4000-8000-00000000a102";
const LOST = "00000000-0000-4000-8000-00000000a103";
const QUEUED = "00000000-0000-4000-8000-00000000a104";
const SECRET = "s3cr3t-never-in-a-step";
const here = { project: null, environment: null };

const admitted = (deploymentId: string) => ({
  name: configDeploymentAdmittedEvent,
  data: { organizationId: ORGANIZATION, environmentId: ENVIRONMENT, deploymentId },
});

it.live(
  "Cloud's worker runs an admitted Deployment once, honours a cancel, and no step carries its secrets",
  () =>
    Effect.gen(function* () {
      const services = yield* Layer.build(yield* storeTestCloud());
      yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      const store = yield* cloudStore.pipe(Effect.provide(services));
      const write = (command: ConfigCommand) => Effect.promise(() => store.write(ORGANIZATION, command));
      yield* write({ command: "create_project", id: PROJECT, name: "shop", default_environment: ENVIRONMENT });
      yield* write({
        command: "create_service", id: SERVICE, environment: here, name: "web", image: "nginx:1",
      });
      yield* write({
        command: "edit", environment: here, expect: null, changes: [
          { op: "set", path: "web.env.TOKEN", value: { secret: SECRET } },
          { op: "set", path: "web.env.GREETING", value: "hi-${{ TOKEN }}" },
        ],
      });
      yield* write({ command: "admit", admit: "deploy", id: DEPLOYED, environment: here, services: [], version: null, accept_volume_loss: [] });

      const runner: Parameters<typeof createRunStoreDeployment>[1] =
        makeInngestEffectRunner((program) => Effect.runPromise(program.pipe(Effect.provide(services))));
      const inngest = new Inngest({ id: "store-deployment-test" });
      const worker = createRunStoreDeployment(inngest, runner);
      const run = (deploymentId: string) =>
        Effect.promise(() => new InngestTestEngine({ function: worker, events: [admitted(deploymentId)] }).execute());

      // No Server is enrolled, so its runner records that nothing ran.
      const first = yield* run(DEPLOYED);
      expect(first.result).toMatchObject({ ran: { id: DEPLOYED, status: "failed" } });
      const view = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "deployment", id: DEPLOYED }));
      expect(view).toMatchObject({
        status: "failed",
        outcome: { type: "not_executed", reason: "No Server is enrolled in this Organization" },
      });

      // A duplicate delivery is another runner: it finds the Deployment ended and runs nothing.
      const duplicate = yield* run(DEPLOYED);
      expect(duplicate.result).toMatchObject({ nothingToRun: expect.any(String) });

      // A cancelled Deployment never runs.
      yield* write({ command: "admit", admit: "deploy", id: CANCELLED, environment: here, services: [], version: null, accept_volume_loss: [] });
      yield* write({ command: "cancel", deployment: CANCELLED });
      const cancelled = yield* run(CANCELLED);
      expect(cancelled.result).toMatchObject({ nothingToRun: expect.any(String) });

      // A failed run that never claimed its Deployment leaves it to the runner that did.
      const onFailure = worker.opts.onFailure;
      if (onFailure === undefined) return expect.fail("the worker records abandoned runs");
      const abandoned = yield* Effect.promise(() =>
        // SAFETY: the failure handler reads only the failed run's ID and its triggering event.
        Promise.resolve(onFailure({ event: { data: { run_id: "never-claimed", event: admitted(DEPLOYED) } } } as never)));
      expect(abandoned).toMatchObject({ nothingToRun: expect.any(String) });

      // A run cancelled in Inngest after it claimed its Deployment records that it stopped: it never reads running.
      yield* write({ command: "admit", admit: "deploy", id: LOST, environment: here, services: [], version: null, accept_volume_loss: [] });
      yield* recordStoreDeploymentRun("cancelled-run", admitted(LOST).data).pipe(Effect.provide(services));
      yield* Effect.promise(() => store.runDeployment(ORGANIZATION, LOST, "cloud-cancelled-run", [], { failure: "stopped" }));
      const stop = (runId: string) => Effect.promise(async () => (await new InngestTestEngine({
        function: createCancelStoreDeployment(inngest, runner),
        events: [{ name: "inngest/function.cancelled", data: { function_id: "run-store-deployment", run_id: runId } }],
      }).execute()).result);
      expect(yield* stop("cancelled-run")).toMatchObject({ abandoned: { id: LOST } });
      // Recorded runs are forgotten once stopped, and other functions' runs were never recorded.
      expect(yield* stop("cancelled-run")).toEqual({ skipped: true });

      // An admission whose hand-off was lost is handed over again once it has waited a minute, unless a worker run
      // holds its Environment's queue.
      yield* write({ command: "admit", admit: "deploy", id: QUEUED, environment: here, services: [], version: null, accept_volume_loss: [] });
      const unclaimed = (at: Date) => unclaimedStoreDeployments(at).pipe(
        Effect.provide(services), Effect.map((found) => found.map((deployment) => deployment.deploymentId)));
      const later = new Date(Date.now() + 2 * 60_000);
      expect(yield* unclaimed(later)).toContain(QUEUED);
      expect(yield* unclaimed(new Date())).toEqual([]);
      yield* recordStoreDeploymentRun("waiting-run", admitted(QUEUED).data).pipe(Effect.provide(services));
      expect(yield* unclaimed(later)).toEqual([]);

      // Every serialized step and result holds only summaries: never a secret or a Deploy Intent.
      const steps = yield* Effect.promise(() =>
        Promise.all([first, duplicate, cancelled].flatMap((output) => Object.values(output.state))));
      const serialized = JSON.stringify([first.result, duplicate.result, cancelled.result, steps]);
      expect(serialized).not.toContain(SECRET);
      expect(serialized).not.toContain("hi-");
      expect(serialized).not.toContain("resolvedEnv");
      expect(serialized).not.toContain("ployz1:");
    }),
  60_000,
);

const HEAD = "a".repeat(40);
const archive = (() => {
  const header = new Header({ path: "root/package.json", size: 0, mode: 0o644, type: "File" });
  header.encode();
  return gzipSync(Buffer.concat([Buffer.from(header.block ?? Buffer.alloc(512)), Buffer.alloc(1024)]));
})();

/** GitHub with the public `acme/web`, whose `main` is at `HEAD` unless `branchGone`. */
function github(state: { branchGone: boolean; heads: number; archives: string[] }): GithubApiService {
  return {
    json: (request) => {
      if (request.url.includes("/git/ref/")) {
        state.heads += 1;
        if (state.branchGone) {
          return Effect.fail(new GithubObservationError({ code: "not_found", operation: request.operation, retriable: false, message: "gone" }));
        }
        return Schema.decodeUnknownEffect(request.schema)({ ref: "refs/heads/main", object: { type: "commit", sha: HEAD } }).pipe(Effect.orDie);
      }
      return Schema.decodeUnknownEffect(request.schema)({ id: 42, full_name: "acme/web", private: false }).pipe(Effect.orDie);
    },
    archive: ({ sha }) => {
      state.archives.push(sha);
      return Effect.succeed(new Response(archive));
    },
  };
}

it.live(
  "Cloud's worker pins a Git Service's commit once and checks it out for the runner; a source it can't read is why nothing ran",
  () =>
    Effect.gen(function* () {
      const state = { branchGone: false, heads: 0, archives: [] as string[] };
      const services = yield* Layer.build(yield* storeTestCloud({ github: github(state) }));
      yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      const store = yield* cloudStore.pipe(Effect.provide(services));
      const write = (command: ConfigCommand, trusted?: ConfigTrusted) =>
        Effect.promise(() => store.write(ORGANIZATION, command, trusted));
      yield* write({ command: "create_project", id: PROJECT, name: "shop", default_environment: ENVIRONMENT });
      yield* write({ command: "create_git_service", id: SERVICE, environment: here, name: "web", repository: "acme/web", branch: null }, {
        repositories: [{ repository: "acme/web", repository_id: 42, access: { type: "public" }, default_branch: "main", branches: [] }],
        domains: { custom_domains: false, cluster_domain: null, certificates: null, ingress_addresses: [], lookups: [] },
      });
      yield* write({ command: "admit", admit: "deploy", id: DEPLOYED, environment: here, services: [], version: null, accept_volume_loss: [] });

      const worker = createRunStoreDeployment(
        new Inngest({ id: "store-deployment-git-test" }),
        makeInngestEffectRunner((program) => Effect.runPromise(program.pipe(Effect.provide(services)))),
      );
      const run = (deploymentId: string) =>
        Effect.promise(() => new InngestTestEngine({ function: worker, events: [admitted(deploymentId)] }).execute());
      const view = (deploymentId: string) =>
        Effect.promise(() => store.read(ORGANIZATION, { query: "deployment", id: deploymentId }));

      // The branch head is pinned and checked out; with no Server enrolled, nothing ran.
      const first = yield* run(DEPLOYED);
      expect(first.result).toMatchObject({ ran: { id: DEPLOYED, status: "failed" } });
      expect(yield* view(DEPLOYED)).toMatchObject({
        outcome: { type: "not_executed", reason: "No Server is enrolled in this Organization" },
        // Auto tries GitHub first; a public repository has no GitHub App to run it, so the servers take it.
        builds: [{ service: "web", commit: HEAD, status: "pending", message: "GitHub builds need the Ployz GitHub App on acme/web" }],
      });
      expect(state).toMatchObject({ heads: 1, archives: [HEAD] });

      // Another delivery reads the pin; it never asks GitHub where the branch is now.
      yield* run(DEPLOYED);
      expect(state.heads).toBe(1);

      // A branch GitHub no longer has is why the next Deployment ran nothing.
      state.branchGone = true;
      yield* write({ command: "admit", admit: "deploy", id: CANCELLED, environment: here, services: [], version: null, accept_volume_loss: [] });
      yield* run(CANCELLED);
      expect(yield* view(CANCELLED)).toMatchObject({
        status: "failed",
        outcome: { type: "not_executed", reason: "The source branch no longer exists." },
        builds: [],
      });
    }),
  60_000,
);
