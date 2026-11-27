import { FULL_GRANT } from "#/modules/identity/token-grant";
import { it } from "@effect/vitest";
import type { ConfigCommand } from "@ployz/sdk";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { expect, vi } from "vitest";
import { cancelStoreGithubBuilds, checkStoreGithubBuild } from "#/modules/config-store/store-github-builds.server";
import { failStoreDeploymentUnrun, recordStoreDeploymentRun, type StoreServices } from "#/modules/config-store/store-deployment.server";
import type { EffectRunner } from "#/modules/inngest/run.server";
import { disconnectGithub } from "#/modules/config-store/store-github-repositories.server";
import { executeProcessGithubInstallationReceived } from "#/modules/github/inngest-sync/webhook";
import type { Caller } from "#/modules/identity/actor";
import type { PloyzStepTools } from "#/modules/inngest/client";
import { githubBuildRunCompleted, githubInstallationReceived } from "#/modules/github/github.events";
import { fakeGithubApi, fakeGithubAuthority } from "#/test/fake-github";
import { acmeWeb, seedStoreGitService, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000c001";
const DEPLOYMENT = "00000000-0000-4000-8000-00000000c101";
const BUILD = `${DEPLOYMENT}.web`;
const RUN = 9301;
const CANCEL = { operation: "cancelBuild", input: { installationId: 7, repositoryId: 42, runId: RUN } };
const here = { project: null, environment: null };

const step: Pick<PloyzStepTools, "run" | "sendEvent"> = {
  run: async (_id, operation, ...input) => {
    const result = await operation(...input);
    return result === undefined ? null : JSON.parse(JSON.stringify(result));
  },
  sendEvent: async () => ({ ids: [] }),
};

const webhook = (action: "suspend" | "deleted") => ({
  name: githubInstallationReceived.name,
  data: {
    deliveryId: `delivery-${action}`, action, sender: { id: 9001, login: "ada" },
    installation: { id: 7, account: { login: "acme", type: "Organization", avatar_url: "https://avatars.githubusercontent.com/u/7" } },
  },
});

const cases = [
  { stop: "disconnect", failed: "GitHub account `acme` was disconnected" },
  { stop: "suspend", failed: "GitHub account `acme` was suspended" },
  { stop: "deleted", failed: "GitHub account `installation 7` was disconnected" },
] as const;

for (const { stop, failed } of cases) {
  it.live(`a ${stop} cancels the building run as Cloud and wakes its walk, which fails the build at once`, () =>
    Effect.gen(function* () {
      const github = fakeGithubApi({ "/repos/acme/web": { id: 42, full_name: "acme/web" } });
      const authority = fakeGithubAuthority({ cancelBuild: () => Effect.void });
      const inngest = new Inngest({ id: "store-github-stop" });
      const sent = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const services = yield* storeTestCloud({ github: github.service, authority: authority.service, inngest });
      const provided = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(Effect.provide(services));
      const run: EffectRunner<StoreServices> = (program) => Effect.runPromise(provided(program));
      const userId = yield* provided(seedStoreOrganization(ORGANIZATION));
      const store = yield* provided(seedStoreGitService(ORGANIZATION, {
        project: "00000000-0000-4000-8000-00000000c002", environment: "00000000-0000-4000-8000-00000000c003", service: "00000000-0000-4000-8000-00000000c004",
      }));
      const write = (command: ConfigCommand) => Effect.promise(() => store.write(ORGANIZATION, command, acmeWeb()));
      yield* write({ command: "admit", admit: "deploy", id: DEPLOYMENT, environment: here, services: [], version: null, accept_volume_loss: [] });
      yield* Effect.promise(() => store.pinSources(DEPLOYMENT, { web: "c".repeat(40) }));
      yield* Effect.promise(() => store.githubBegin(DEPLOYMENT, "cloud-walk-1"));
      yield* Effect.promise(() => store.githubDispatched(BUILD, {
        run_id: RUN, run_url: `https://github.com/acme/web/actions/runs/${RUN}`, workflow_ref: "acme/web/.github/workflows/ployz-build.yml@refs/heads/main",
        repository: "acme/web", installation_id: 7, push: false, attempt: "cloud-walk-1",
      }));
      yield* provided(recordStoreDeploymentRun("walk-1", { organizationId: ORGANIZATION, environmentId: "00000000-0000-4000-8000-00000000c003", deploymentId: DEPLOYMENT }));

      if (stop === "disconnect") {
        const caller: Caller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "token", id: "token-1", grant: FULL_GRANT }, role: "owner", support: null, via: "cli" };
        yield* provided(disconnectGithub(caller, "shop", "acme"));
      } else {
        yield* Effect.promise(() => executeProcessGithubInstallationReceived({ event: webhook(stop), step }, run, run));
      }

      // Only the walk's stop cancels: it owes the run its cancel in the write that ends the build.
      const cancels = () => authority.calls.filter((call) => call.operation === "cancelBuild");
      expect(cancels()).toEqual([]);
      expect(sent.mock.calls).toEqual([[{ id: `stopped-${BUILD}`, name: githubBuildRunCompleted.name, data: { runId: RUN } }]]);

      const target = { build: BUILD, service: "web", repository: "acme/web", repositoryId: 42, installationId: 7, hasNext: true };
      expect(yield* provided(checkStoreGithubBuild(ORGANIZATION, target, { ended: true, startLimit: false }, "cloud-walk-1", RUN))).toEqual({ failed });
      // The walk fails the Deployment with why, and its stop then ends the build GitHub held.
      yield* provided(failStoreDeploymentUnrun(ORGANIZATION, "00000000-0000-4000-8000-00000000c003", DEPLOYMENT, "walk-1", failed));
      yield* provided(cancelStoreGithubBuilds(DEPLOYMENT, "cloud-walk-1"));
      expect(yield* Effect.promise(() => store.githubBuild(BUILD))).toMatchObject({ held: { status: "failed" } });
      expect(cancels()).toEqual([CANCEL]);
      expect(github.calls.map((call) => call.access)).toEqual([{ type: "installation", installationId: 7 }]);
    }), 60_000);
}
