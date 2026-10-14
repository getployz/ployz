import { it } from "@effect/vitest";
import type { ConfigCommand } from "@ployz/sdk";
import { Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { expect, vi } from "vitest";
import { checkStoreGithubBuild } from "#/modules/config-store/store-github-builds.server";
import { recordStoreDeploymentRun, type StoreEffectRunner } from "#/modules/config-store/store-deployment.server";
import { disconnectGithub } from "#/modules/github/github-cli.server";
import { executeProcessGithubInstallationReceived } from "#/modules/github/inngest-sync/webhook";
import type { Caller } from "#/modules/identity/actor";
import type { PloyzStepTools } from "#/modules/inngest/client";
import { githubBuildRunCompletedEvent, githubInstallationReceivedEvent } from "#/modules/inngest/events";
import { fakeGithubApiBy } from "#/test/fake-github";
import { acmeWeb, seedStoreGitService, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000c001";
const DEPLOYMENT = "00000000-0000-4000-8000-00000000c101";
const BUILD = `${DEPLOYMENT}.web`;
const RUN = 9301;
const CANCEL = `/repos/acme/web/actions/runs/${RUN}/cancel`;
const here = { project: null, environment: null };

const step: Pick<PloyzStepTools, "run" | "sendEvent"> = {
  run: async (_id, operation, ...input) => {
    const result = await operation(...input);
    return result === undefined ? null : JSON.parse(JSON.stringify(result));
  },
  sendEvent: async () => ({ ids: [] }),
};

const webhook = (action: "suspend" | "deleted") => ({
  name: githubInstallationReceivedEvent,
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
      const github = fakeGithubApiBy(({ path }) => path === CANCEL ? {} : undefined);
      const inngest = new Inngest({ id: "store-github-stop" });
      const sent = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const services = yield* Layer.build(yield* storeTestCloud({ github: github.service, inngest }));
      const provided = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(Effect.provide(services));
      const run: StoreEffectRunner = (program) => Effect.runPromise(provided(program));
      const userId = yield* provided(seedStoreOrganization(ORGANIZATION));
      const store = yield* provided(seedStoreGitService(ORGANIZATION, {
        project: "00000000-0000-4000-8000-00000000c002", environment: "00000000-0000-4000-8000-00000000c003", service: "00000000-0000-4000-8000-00000000c004",
      }));
      const write = (command: ConfigCommand) => Effect.promise(() => store.write(ORGANIZATION, command, acmeWeb()));
      yield* write({ command: "admit", admit: "deploy", id: DEPLOYMENT, environment: here, services: [], version: null, accept_volume_loss: [] });
      yield* Effect.promise(() => store.pinSources(DEPLOYMENT, { web: "c".repeat(40) }));
      yield* Effect.promise(() => store.githubDispatched(BUILD, {
        run_id: RUN, run_url: `https://github.com/acme/web/actions/runs/${RUN}`, workflow_ref: "acme/web/.github/workflows/ployz-build.yml@refs/heads/main",
        repository: "acme/web", installation_id: 7,
      }));
      yield* provided(recordStoreDeploymentRun("walk-1", { organizationId: ORGANIZATION, environmentId: "00000000-0000-4000-8000-00000000c003", deploymentId: DEPLOYMENT }));

      if (stop === "disconnect") {
        const caller: Caller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "token", id: "token-1" }, role: "owner", support: null, via: "cli" };
        yield* provided(disconnectGithub(caller, 7));
      } else {
        yield* Effect.promise(() => executeProcessGithubInstallationReceived({ event: webhook(stop), step }, run, run));
      }

      const cancels = () => github.calls.filter((call) => call.path === CANCEL);
      expect(cancels()).toEqual([{ path: CANCEL, access: { type: "installation", installationId: 7 } }]);
      expect(sent.mock.calls).toEqual([[{ id: `stopped-${BUILD}`, name: githubBuildRunCompletedEvent, data: { runId: RUN } }]]);

      const target = { build: BUILD, service: "web", repository: "acme/web", repositoryId: 42, installationId: 7, hasNext: true };
      expect(yield* provided(checkStoreGithubBuild(ORGANIZATION, target, { ended: true, startLimit: false }))).toEqual({ failed });
      expect(yield* Effect.promise(() => store.githubBuild(BUILD))).toMatchObject({ status: "failed" });
      expect(cancels()).toEqual([
        { path: CANCEL, access: { type: "installation", installationId: 7 } },
        { path: CANCEL, access: { type: "installation", installationId: 7 } },
      ]);
    }), 60_000);
}
