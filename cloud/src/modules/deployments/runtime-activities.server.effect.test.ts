import type { Client, ContainerId, DeployIntent, DeployOutcome, ExecutionError, PreparedDeploy } from "@ployz/sdk";
import { assert, it } from "@effect/vitest";
import { Effect, Fiber, Layer, Redacted } from "effect";
import { resolvedServiceSpecFixture, runtimeWatchMachineFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";
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
  executeRuntimeIntent,
  previewRuntimeIntent,
} from "./runtime-activities.server";
import { compileSdkDeployIntent } from "./runtime-preview";

const preview = {
  storage: [],
  prune_refusal: null,
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

it.effect("prepares once and executes that handle before closing the session", () =>
  Effect.gen(function* () {
    let closed = 0;
    let preparedCount = 0;
    let executedCount = 0;
    const outcome = { type: "success" as const, completed: [] };
    const prepared = asTestDouble<PreparedDeploy>()({
      ...preview,
      noop: false,
      confirm: () => {
        assert.strictEqual(closed, 0);
        executedCount += 1;
        return ({
        abort: () => undefined,
        finished: Promise.resolve(outcome),
        async *[Symbol.asyncIterator]() {
          yield { type: "outcome" as const, outcome };
        },
      });
      },
    });
    const client = asTestDouble<Client>()({
      preview: async () => {
        preparedCount += 1;
        return prepared;
      },
      close: async () => undefined,
    });

    const result = yield* Effect.scoped(
      executeRuntimeIntent("organization-1", intent()),
    ).pipe(Effect.provide(runtimeLayer(client, () => (closed += 1))));

    assert.deepStrictEqual(result.outcome, { type: "success", completed: 0 });
    assert.deepStrictEqual(result.preview, preview);
    assert.strictEqual(preparedCount, 1);
    assert.strictEqual(executedCount, 1);
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
      executeRuntimeIntent("organization-1", intent()),
    ).pipe(Effect.provide(runtimeLayer(client, () => (closed += 1))));
    const fiber = yield* program.pipe(Effect.forkChild);

    yield* Effect.promise(() => started);
    yield* Fiber.interrupt(fiber);

    assert.strictEqual(aborted, 1);
    assert.strictEqual(closed, 1);
  }),
);

it.effect("retains complete partial evidence privately without exposing operation inputs", () =>
  Effect.gen(function* () {
    let preparations = 0;
    let executions = 0;
    const spec = resolvedServiceSpecFixture();
    spec.container.environment = { PASSWORD: "never-publish" };
    const machineId = (id: string) => runtimeWatchMachineFixture(id.repeat(32), id).id;
    const operation = { type: "run_container" as const, machine_id: machineId("a"), spec, skip_health_monitor: false };
    const outcome: DeployOutcome<ExecutionError> = {
      type: "failed" as const,
      completed: [operation],
      failed: {
        type: "replacement_health",
        operation: { machine_id: machineId("b"), old_container_id: "b".repeat(64) as ContainerId, spec, skip_health_monitor: false },
        error: { type: "cancelled" },
        compensation: { type: "stop_first", stop_new_container: { type: "stopped" }, restart_old_container: { type: "restarted" } },
      },
      unexecuted: [{ ...operation, machine_id: machineId("c") }, { ...operation, machine_id: machineId("d") }],
    };
    const failedOperation = { type: "replace_container" as const, ...outcome.failed.operation };
    const operations = [operation, failedOperation, ...outcome.unexecuted];
    const prepared = asTestDouble<PreparedDeploy>()({
      ...preview,
      operations: operations.map((operation, index) => ({
        index, operation, machine_id: operation.type === "remove_volume" ? operation.id.machine_id : operation.machine_id, service_name: spec.name, machine_name: null, display_name: null,
        status: { type: "pending" as const },
      })),
      confirm: () => {
        executions += 1;
        return {
          abort: () => undefined,
          finished: Promise.resolve(outcome),
          async *[Symbol.asyncIterator]() { yield { type: "outcome", outcome }; },
        };
      },
    });
    const client = asTestDouble<Client>()({
      preview: async () => { preparations += 1; return prepared; },
      close: async () => undefined,
    });
    const result = yield* Effect.scoped(executeRuntimeIntent("organization-1", intent()))
      .pipe(Effect.provide(runtimeLayer(client, () => undefined)));
    assert.deepStrictEqual(result.outcome, { type: "failed", completed: 1, unexecuted: 2, reason: "cancelled" });
    assert.deepStrictEqual(Redacted.value(result.evidence), { version: 1, outcome });
    assert.strictEqual(preparations, 1);
    assert.strictEqual(executions, 1);
    assert.isFalse(JSON.stringify(result).includes("never-publish"));
  }),
);
