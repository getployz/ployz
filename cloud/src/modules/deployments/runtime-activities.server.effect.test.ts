import type { Client, DeployIntent, PreparedDeploy } from "@ployz/sdk";
import { assert, it } from "@effect/vitest";
import { Effect, Fiber, Layer } from "effect";
import { asTestDouble } from "#/lib/test-double";
import type { DeploymentContext } from "#/modules/deployments/runtime-repository.server";
import {
  createDefaultServiceHealthcheck,
  createDefaultServiceRestartPolicy,
  createImageServiceSource,
  projectServiceDeploymentConfig,
} from "#/modules/environment-design/services";
import type { DialTenant } from "#/modules/runtime/dial-entry";
import { makeOrganizationRuntimeLayer } from "#/modules/runtime/organization-runtime.server";
import {
  makePloyzLayer,
  PloyzProviderError,
} from "#/modules/runtime/ployz.server";
import {
  confirmRuntimeIntent,
  previewRuntimeIntent,
} from "./runtime-activities.server";
import { compileSdkDeployIntent } from "./runtime-preview";

const preview = {
  project_name: "production",
  operations: [],
  warnings: [],
  would_remove: [],
  volumes_to_create: [],
  preserved_volumes: [],
};

const tenant = {
  relayUrl: "wss://relay.example.test",
  bearer: "tenant-token",
  pairing: "ppair_test",
  preferredMachineId: "machine-a",
  enrolledMachineIds: ["machine-a"],
} satisfies DialTenant;

function context(deployPreview: typeof preview | null = null) {
  return {
    deployment: {
      id: "deployment-1",
      environmentId: "environment-1",
      status: "planning",
      deployPreview,
    },
    environment: { id: "environment-1", namespace: "production" },
    project: { id: "project-1", organizationId: "organization-1" },
    organization: { id: "organization-1", slug: "acme" },
    snapshots: [
      {
        serviceId: "service-1",
        serviceSlug: "api",
        config: projectServiceDeploymentConfig({
          name: "API",
          source: createImageServiceSource({ image: "nginx:1.27" }),
          preDeployCommand: null,
          startCommand: null,
          healthcheck: createDefaultServiceHealthcheck(),
          restartPolicy: createDefaultServiceRestartPolicy(),
          privateDns: "api",
        }),
      },
    ],
    volumes: [],
  } satisfies DeploymentContext;
}

function runtimeLayer(client: Client, finalized: () => void) {
  return makeOrganizationRuntimeLayer(() =>
    Effect.succeed({ kind: "ready", tenant }),
  ).pipe(
    Layer.provide(
      makePloyzLayer({
        connect: async () =>
          asTestDouble<Client>()({
            preview: (intent: DeployIntent) => client.preview(intent),
            close: async () => {
              await client.close();
              finalized();
            },
          }),
      }),
    ),
  );
}

function intent() {
  const deployment = context();
  return compileSdkDeployIntent({
    projectName: deployment.environment.namespace,
    snapshots: deployment.snapshots.map((snapshot) => ({
      ...snapshot,
      resolvedEnv: {},
    })),
    volumes: deployment.volumes,
  });
}

it.effect("strictly decodes a preview and closes its scoped session", () =>
  Effect.gen(function* () {
    let closed = 0;
    const client = asTestDouble<Client>()({
      preview: async () => ({ ...preview, noop: false }),
      close: async () => undefined,
    });

    const result = yield* Effect.scoped(
      previewRuntimeIntent("organization-1", intent()),
    ).pipe(Effect.provide(runtimeLayer(client, () => (closed += 1))));

    assert.deepStrictEqual(result.preview, preview);
    assert.strictEqual(closed, 1);
  }),
);

it.effect("closes the session when preview fails with a typed provider error", () =>
  Effect.gen(function* () {
    let closed = 0;
    const client = asTestDouble<Client>()({
      preview: async () => {
        throw new Error("runtime unavailable");
      },
      close: async () => undefined,
    });
    const error = yield* Effect.scoped(
      previewRuntimeIntent("organization-1", intent()),
    ).pipe(
      Effect.provide(runtimeLayer(client, () => (closed += 1))),
      Effect.flip,
    );

    assert.instanceOf(error, PloyzProviderError);
    assert.strictEqual(closed, 1);
  }),
);

it.effect("closes the confirmation watch and session after its outcome", () =>
  Effect.gen(function* () {
    let closed = 0;
    const outcome = { type: "success" as const, completed: [] };
    const prepared = asTestDouble<PreparedDeploy>()({
      ...preview,
      noop: false,
      confirm: () => ({
        abort: () => undefined,
        finished: Promise.resolve(outcome),
        async *[Symbol.asyncIterator]() {
          yield { type: "outcome" as const, outcome };
        },
      }),
    });
    const client = asTestDouble<Client>()({
      preview: async () => prepared,
      close: async () => undefined,
    });

    const result = yield* Effect.scoped(
      confirmRuntimeIntent("organization-1", intent(), preview),
    ).pipe(Effect.provide(runtimeLayer(client, () => (closed += 1))));

    assert.deepStrictEqual(result, { type: "success" });
    assert.strictEqual(closed, 1);
  }),
);

it.effect("aborts an interrupted confirmation watch before closing the session", () =>
  Effect.gen(function* () {
    let closed = 0;
    let aborted = 0;
    let startedResolve: () => void = () => undefined;
    const started = new Promise<void>((resolve) => {
      startedResolve = resolve;
    });
    const prepared = asTestDouble<PreparedDeploy>()({
      ...preview,
      noop: false,
      confirm: () => ({
        abort: () => {
          aborted += 1;
        },
        finished: new Promise<never>(() => undefined),
        async *[Symbol.asyncIterator]() {
          startedResolve();
          yield* [];
          await new Promise<never>(() => undefined);
        },
      }),
    });
    const client = asTestDouble<Client>()({
      preview: async () => prepared,
      close: async () => undefined,
    });
    const program = Effect.scoped(
      confirmRuntimeIntent("organization-1", intent(), preview),
    ).pipe(Effect.provide(runtimeLayer(client, () => (closed += 1))));
    const fiber = yield* program.pipe(Effect.forkChild);

    yield* Effect.promise(() => started);
    yield* Fiber.interrupt(fiber);

    assert.strictEqual(aborted, 1);
    assert.strictEqual(closed, 1);
  }),
);
