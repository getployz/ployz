import type { Client, Connection } from "@ployz/sdk";
import { assert, it } from "@effect/vitest";
import { Deferred, Effect, Fiber } from "effect";
import { asTestDouble } from "#/lib/test-double";
import { MissingDataLossIdentities } from "#/modules/runtime/data-loss-confirm";
import {
  makePloyzLayer,
  Ployz,
  PloyzProviderError,
  PloyzPreparationError,
} from "#/modules/runtime/ployz.server";

const options = {
  connections: [{ management: "ployz1:candidate" }] satisfies Connection[],
};

it.effect("scopes each connected Ployz session", () =>
  Effect.gen(function* () {
    let opened = 0;
    const signals: AbortSignal[] = [];
    let closed = 0;
    const layer = makePloyzLayer({
      connect: async (options) => {
        if (!("signal" in options) || !options.signal) throw new Error("missing connection signal");
        signals.push(options.signal);
        opened += 1;
        return asTestDouble<Client>()({
          close: async () => {
            closed += 1;
          },
        });
      },
    });

    yield* Effect.scoped(
      Effect.gen(function* () {
        const ployz = yield* Ployz;
        yield* ployz.connect(options);
        assert.strictEqual(opened, 1);
        assert.isFalse(signals[0]?.aborted);
        assert.strictEqual(closed, 0);
      }),
    ).pipe(Effect.provide(layer));

    assert.strictEqual(closed, 1);
    assert.isTrue(signals[0]?.aborted);
  }),
);

it.effect("classifies provider connection failures", () =>
  Effect.gen(function* () {
    const layer = makePloyzLayer({
      connect: async () => {
        throw new Error("token=provider-secret");
      },
    });

    const error = yield* Effect.scoped(
      Effect.gen(function* () {
        const ployz = yield* Ployz;
        return yield* ployz.connect(options);
      }),
    ).pipe(Effect.provide(layer), Effect.flip);

    assert.instanceOf(error, PloyzProviderError);
    assert.strictEqual(error.operation, "connect");
    assert.instanceOf(error.cause, Error);
  }),
);

it.effect("passes shipped project and cluster teardown methods through", () =>
  Effect.gen(function* () {
    const projectDataLoss = { data_loss: [] };
    const projectOutcome = { type: "success" as const, completed: [] };
    const clusterDataLoss = { data_loss: [] };
    const clusterOutcome = {
      destroyed_projects: [],
      machines: { successes: [], failures: [], omissions: [] },
      pairing_revoked: true,
    };
    const calls: unknown[] = [];
    const client = asTestDouble<Client>()({
      dataLossIfProjectDestroyed: async (
        ...args: Parameters<Client["dataLossIfProjectDestroyed"]>
      ) => {
        calls.push(["project data loss", args]);
        return projectDataLoss;
      },
      destroyProject: async (...args: Parameters<Client["destroyProject"]>) => {
        calls.push(["destroy project", args]);
        return projectOutcome;
      },
      dataLossIfClusterDestroyed: async () => {
        calls.push(["cluster data loss"]);
        return clusterDataLoss;
      },
      destroyCluster: async (...args: Parameters<Client["destroyCluster"]>) => {
        calls.push(["destroy cluster", args]);
        return clusterOutcome;
      },
      close: async () => undefined,
    });
    const layer = makePloyzLayer({
      connect: async () => client,
    });
    const confirmation = { confirmed: [] };

    yield* Effect.scoped(
      Effect.gen(function* () {
        const session = yield* (yield* Ployz).connect(options);
        assert.deepStrictEqual(
          yield* session.dataLossIfProjectDestroyed("app", true),
          projectDataLoss,
        );
        assert.deepStrictEqual(
          yield* session.destroyProject("app", confirmation, true),
          projectOutcome,
        );
        assert.deepStrictEqual(
          yield* session.dataLossIfClusterDestroyed(),
          clusterDataLoss,
        );
        assert.deepStrictEqual(
          yield* session.destroyCluster(confirmation),
          clusterOutcome,
        );
      }),
    ).pipe(Effect.provide(layer));

    assert.deepStrictEqual(calls, [
      ["project data loss", ["app", true]],
      ["destroy project", ["app", confirmation, true]],
      ["cluster data loss"],
      ["destroy cluster", [confirmation]],
    ]);
  }),
);

it.effect("preserves exact execute-time Data Loss refusals", () =>
  Effect.gen(function* () {
    const missing = [
      {
        kind: "docker_volume" as const,
        id: {
          machine_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          name: "new-data",
        },
      },
    ];
    const client = asTestDouble<Client>()({
      destroyCluster: async () => {
        throw Object.assign(new Error("confirmation is stale"), {
          code: "invalid_argument",
          details: { missing },
        });
      },
      close: async () => undefined,
    });
    const layer = makePloyzLayer({
      connect: async () => client,
    });

    const error = yield* Effect.scoped(
      Effect.gen(function* () {
        const session = yield* (yield* Ployz).connect(options);
        return yield* session.destroyCluster({ confirmed: [] });
      }),
    ).pipe(Effect.provide(layer), Effect.flip);

    assert.instanceOf(error, MissingDataLossIdentities);
    assert.deepStrictEqual(error.identities, missing);
  }),
);


it.effect("cancels an in-flight SDK connection when its fiber is interrupted", () =>
  Effect.gen(function* () {
    const started = yield* Deferred.make<AbortSignal>();
    const layer = makePloyzLayer({
      connect: (options) => new Promise<Client>((_resolve, reject) => {
        if (!("signal" in options) || !options.signal) throw new Error("missing connection signal");
        const signal = options.signal;
        signal.addEventListener("abort", () => reject(new Error("cancelled")), { once: true });
        Effect.runSync(Deferred.succeed(started, signal));
      }),
    });
    const fiber = yield* Effect.scoped(
      Effect.flatMap(Ployz, (ployz) => ployz.connect(options)),
    ).pipe(Effect.provide(layer), Effect.forkChild);
    const signal = yield* Deferred.await(started);
    assert.isFalse(signal.aborted);
    yield* Fiber.interrupt(fiber);
    assert.isTrue(signal.aborted);
  }),
);

it.effect("forwards progress and the finished outcome, aborting if the evidence consumer fails", () =>
  Effect.gen(function* () {
    for (const failConsumer of [false, true]) {
      let aborted = false;
      const received: string[] = [];
      const outcome = { type: "success" as const, completed: [] };
      const layer = makePloyzLayer({ connect: async () => asTestDouble<Client>()({
        preview: async () => ({
          noop: false, project_name: "test", storage: [], prune_refusal: null, operations: [], warnings: [], would_remove: [], volumes_to_create: [], preserved_volumes: [],
          confirm: () => ({ abort: () => { aborted = true; }, finished: Promise.resolve(outcome), async *[Symbol.asyncIterator]() { yield { type: "progress" as const, completed: 0, total: 0, rows: [] }; } }),
        }),
        close: async () => undefined,
      }) });
      const result = yield* Effect.scoped(Effect.gen(function* () {
        const session = yield* (yield* Ployz).connect(options);
        const prepared = yield* session.preview(asTestDouble<Parameters<Client["preview"]>[0]>()({}));
        return yield* prepared.confirm(async (event) => {
          received.push(event.type);
          if (failConsumer) throw new Error("Evidence storage unavailable");
        });
      })).pipe(Effect.provide(layer), Effect.result);
      assert.deepStrictEqual(received, failConsumer ? ["progress"] : ["progress", "outcome"]);
      assert.strictEqual(aborted, failConsumer);
      assert.strictEqual(result._tag, failConsumer ? "Failure" : "Success");
    }
  }),
);

it("keeps the session owned through quiet preparation interruption cleanup", async () => {
  let finish: () => void = () => undefined;
  let began: () => void = () => undefined;
  let aborted: () => void = () => undefined;
  const started = new Promise<void>((resolve) => { began = resolve; });
  const stopped = new Promise<void>((resolve) => { aborted = resolve; });
  const finished = new Promise<import("@ployz/sdk").PreparedDeploy>((_resolve, reject) => {
    finish = () => reject({ details: { preparation: { kind: "cancelled" } } });
  });
  void finished.catch(() => undefined);
  let closed = false;
  const layer = makePloyzLayer({ connect: async () => asTestDouble<Client>()({
    prepare: () => {
      began();
      return { abort: () => aborted(), finished,
        async *[Symbol.asyncIterator]() { yield* []; await finished; },
      };
    },
    close: async () => { closed = true; },
  }) });
  const interruption = new AbortController();
  const running = Effect.runPromise(Effect.scoped(Effect.gen(function* () {
    const session = yield* (yield* Ployz).connect(options);
    return yield* session.prepare({ deployment: { projectName: "test", snapshots: [] }, sources: {} }, async () => undefined, new AbortController().signal);
  })).pipe(Effect.provide(layer)), { signal: interruption.signal }).then(() => undefined, () => undefined);
  await started;
  interruption.abort();
  await stopped;
  assert.isFalse(closed);
  finish();
  await running;
  assert.isTrue(closed);
});

it.effect("distinguishes rejected preparation input from a disconnected preparation", () => Effect.gen(function* () {
  for (const [code, failureCode] of [["invalid_argument", "sdk_preparation_failed"], ["unavailable", "sdk_preparation_unknown"]]) {
    const layer = makePloyzLayer({ connect: async () => asTestDouble<Client>()({
      prepare: () => { throw Object.assign(new Error("private-provider-details"), { code }); },
      close: async () => undefined,
    }) });
    const failure = yield* Effect.scoped(Effect.gen(function* () {
      const session = yield* (yield* Ployz).connect(options);
      return yield* session.prepare({ deployment: { projectName: "test", snapshots: [] }, sources: {} }, async () => undefined, new AbortController().signal);
    })).pipe(Effect.provide(layer), Effect.flip);
    assert.instanceOf(failure, PloyzPreparationError);
    if (failure instanceof PloyzPreparationError) assert.strictEqual(failure.failureCode, failureCode);
    assert.isFalse(failure.message.includes("private-provider-details"));
  }
}));

it.effect("retains sanitized terminal diagnosis alongside builder output", () => Effect.gen(function* () {
  const { preparationProgressCollector } = yield* Effect.promise(() => import("#/modules/deployments/preparation-progress"));
  const progress = preparationProgressCollector();
  const output: ReturnType<typeof progress.event>["output"] = [];
  const layer = makePloyzLayer({ connect: async () => asTestDouble<Client>()({
    prepare: () => {
      const finished = Promise.reject({ details: { preparation: {
        kind: "failed", stage: "Building", message: `${"prior context ".repeat(300)}executor exited with code 42; password=hidden; ployz1:capability; deployment-private-value`,
      } } });
      void finished.catch(() => undefined);
      return { abort: () => undefined, finished, async *[Symbol.asyncIterator]() {
        yield { Build: { Output: Array.from(Buffer.alloc(1024, 65)) } };
      } };
    },
    close: async () => undefined,
  }) });
  const failure = yield* Effect.scoped(Effect.gen(function* () {
    const session = yield* (yield* Ployz).connect(options);
    return yield* session.prepare({ deployment: { projectName: "test", snapshots: [asTestDouble<Parameters<Client["prepare"]>[0]["deployment"]["snapshots"][number]>()({ resolvedEnv: { SECRET: "deployment-private-value" } })] }, sources: {} }, async (event) => { output.push(...progress.event(event).output); }, new AbortController().signal);
  })).pipe(Effect.provide(layer), Effect.flip);
  assert.deepEqual(output, [{ step: "build-output", stderr: false, text: "A".repeat(1024) }]);
  assert.instanceOf(failure, PloyzPreparationError);
  assert.include(failure.message, "executor exited with code 42");
  for (const secret of ["hidden", "ployz1:capability", "deployment-private-value"]) assert.notInclude(failure.message, secret);
}));
