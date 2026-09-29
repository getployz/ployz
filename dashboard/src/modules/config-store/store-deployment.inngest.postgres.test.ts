import { testConfigEnvironment } from "#/test/config-environment";
import { it } from "@effect/vitest";
import { InngestTestEngine } from "@inngest/test";
import type { ConfigCommand, ServiceId } from "@ployz/sdk";
import { ConfigProvider, Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { expect } from "vitest";
import { cloudStore } from "#/modules/config-store/config-store.server";
import { createRunStoreDeployment } from "#/modules/config-store/store-deployment.inngest";
import { configDeploymentAdmittedEvent } from "#/modules/inngest/events";
import { AppConfig } from "#/server/config.server";
import { DatabaseLive } from "#/server/database.server";
import { makeInngestEffectRunner } from "#/server/run.server";
import { postgresTestDatabase } from "#/test/postgres";
import { SecretEncryptionLive } from "#/utils/encrypted-secret.server";

const ORGANIZATION = "00000000-0000-4000-8000-00000000a001";
const PROJECT = "00000000-0000-4000-8000-00000000a002";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000a003";
const SERVICE = "00000000-0000-4000-8000-00000000a004";
const DEPLOYED = "00000000-0000-4000-8000-00000000a101";
const CANCELLED = "00000000-0000-4000-8000-00000000a102";
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
      const cloud = yield* postgresTestDatabase;
      const env = { ...testConfigEnvironment(), NODE_ENV: "test", DATABASE_URL: cloud.url.href };
      const configLayer = AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({ env }))));
      const services = yield* Layer.build(Layer.mergeAll(
        configLayer,
        DatabaseLive.pipe(Layer.provide(configLayer)),
        SecretEncryptionLive.pipe(Layer.provide(configLayer)),
      ));
      const store = yield* cloudStore.pipe(Effect.provide(services));
      const write = (command: ConfigCommand) => Effect.promise(() => store.write(ORGANIZATION, command));
      yield* write({ command: "create_project", id: PROJECT, name: "shop", default_environment: ENVIRONMENT });
      yield* write({
        command: "create_service", id: SERVICE as ServiceId, environment: here, name: "web", image: "nginx:1",
      });
      yield* write({
        command: "edit", environment: here, expect: null, changes: [
          { op: "set", path: "web.env.TOKEN", value: { secret: SECRET } },
          { op: "set", path: "web.env.GREETING", value: "hi-${{ TOKEN }}" },
        ],
      });
      yield* write({ command: "admit", id: DEPLOYED, environment: here, services: [], version: null, accept_volume_loss: [] });

      const worker = createRunStoreDeployment(
        new Inngest({ id: "store-deployment-test" }),
        makeInngestEffectRunner((program) => Effect.runPromise(program.pipe(Effect.provide(services)))),
      );
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
      yield* write({ command: "admit", id: CANCELLED, environment: here, services: [], version: null, accept_volume_loss: [] });
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
