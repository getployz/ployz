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

const emptyPreview = {
  noop: false, namespace: "test", storage: [], prune_refusal: null, operations: [],
  warnings: [], would_remove: [], volumes_to_create: [], preserved_volumes: [],
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
    const namespaceDataLoss = { data_loss: [] };
    const namespaceOutcome = { type: "success" as const, completed: [] };
    const clusterDataLoss = { data_loss: [] };
    const clusterOutcome = {
      destroyed_namespaces: [],
      machines: { successes: [], failures: [], omissions: [] },
      pairing_revoked: true,
    };
    const calls: unknown[] = [];
    const client = asTestDouble<Client>()({
      dataLossIfNamespaceDestroyed: async (
        ...args: Parameters<Client["dataLossIfNamespaceDestroyed"]>
      ) => {
        calls.push(["project data loss", args]);
        return namespaceDataLoss;
      },
      destroyNamespace: async (...args: Parameters<Client["destroyNamespace"]>) => {
        calls.push(["destroy namespace", args]);
        return namespaceOutcome;
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
          yield* session.dataLossIfNamespaceDestroyed("app", true),
          namespaceDataLoss,
        );
        assert.deepStrictEqual(
          yield* session.destroyNamespace("app", confirmation, true),
          namespaceOutcome,
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
      ["destroy namespace", ["app", confirmation, true]],
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
          ...emptyPreview,
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

it.effect("types a preview failure as a provider error and still closes the session", () =>
  Effect.gen(function* () {
    let closed = 0;
    const layer = makePloyzLayer({ connect: async () => asTestDouble<Client>()({
      preview: async () => { throw new Error("runtime unavailable"); },
      close: async () => { closed += 1; },
    }) });
    const error = yield* Effect.scoped(Effect.gen(function* () {
      const session = yield* (yield* Ployz).connect(options);
      return yield* session.preview(asTestDouble<Parameters<Client["preview"]>[0]>()({}));
    })).pipe(Effect.provide(layer), Effect.flip);
    assert.instanceOf(error, PloyzProviderError);
    assert.strictEqual(closed, 1);
  }),
);

it.effect("aborts an interrupted confirmation before closing the session", () =>
  Effect.gen(function* () {
    let closed = 0;
    let aborted = 0;
    let began: () => void = () => undefined;
    const started = new Promise<void>((resolve) => { began = resolve; });
    let finish: () => void = () => undefined;
    const finished = new Promise<never>((_resolve, reject) => { finish = () => reject(new Error("Confirmed runner termination")); });
    void finished.catch(() => undefined);
    const layer = makePloyzLayer({ connect: async () => asTestDouble<Client>()({
      preview: async () => ({
        ...emptyPreview,
        confirm: () => ({
          abort: () => { aborted += 1; finish(); },
          finished,
          async *[Symbol.asyncIterator]() {
            began();
            yield* [];
            await new Promise<never>(() => undefined);
          },
        }),
      }),
      close: async () => { closed += 1; },
    }) });
    const fiber = yield* Effect.scoped(Effect.gen(function* () {
      const session = yield* (yield* Ployz).connect(options);
      const prepared = yield* session.preview(asTestDouble<Parameters<Client["preview"]>[0]>()({}));
      return yield* prepared.confirm();
    })).pipe(Effect.provide(layer), Effect.forkChild);
    yield* Effect.promise(() => started);
    yield* Fiber.interrupt(fiber);
    assert.strictEqual(aborted, 1);
    assert.strictEqual(closed, 1);
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
    return yield* session.prepare({ deployment: { namespace: "test", snapshots: [] }, sources: {} }, async () => undefined, new AbortController().signal);
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
      return yield* session.prepare({ deployment: { namespace: "test", snapshots: [] }, sources: {} }, async () => undefined, new AbortController().signal);
    })).pipe(Effect.provide(layer), Effect.flip);
    assert.instanceOf(failure, PloyzPreparationError);
    if (failure instanceof PloyzPreparationError) assert.strictEqual(failure.failureCode, failureCode);
    assert.isFalse(failure.message.includes("private-provider-details"));
  }
}));

it("retains progress persistence failures and waits for the remote cleanup result", async () => {
  for (const confirmed of [true, false]) {
    const storageError = new Error("private SQL parameters");
    let release: () => void = () => undefined;
    let notifyAbort: () => void = () => undefined;
    const aborted = new Promise<void>((resolve) => { notifyAbort = resolve; });
    const finished = new Promise<import("@ployz/sdk").PreparedDeploy>((_resolve, reject) => {
      release = () => reject(confirmed ? { details: { preparation: { kind: "cancelled" } } } : new Error("connection lost"));
    });
    void finished.catch(() => undefined);
    let closed = false;
    const layer = makePloyzLayer({ connect: async () => asTestDouble<Client>()({
      prepare: () => ({ abort: notifyAbort, finished, async *[Symbol.asyncIterator]() { yield { Build: { Stage: "Upload" as const } }; } }),
      close: async () => { closed = true; },
    }) });
    const result = Effect.runPromise(Effect.scoped(Effect.gen(function* () {
      const session = yield* (yield* Ployz).connect(options);
      return yield* session.prepare({ deployment: { namespace: "test", snapshots: [] }, sources: {} }, async () => { throw storageError; }, new AbortController().signal);
    })).pipe(Effect.provide(layer), Effect.flip));
    await aborted;
    assert.isFalse(closed);
    release();
    const failure = await result;
    assert.instanceOf(failure, PloyzPreparationError);
    assert.propertyVal(failure, "cause", storageError);
    assert.propertyVal(failure, "failureCode", confirmed ? "sdk_preparation_failed" : "sdk_preparation_unknown");
    assert.strictEqual(failure.message, "Could not save build progress." + (confirmed ? "" : " Remote work outcome is unknown."));
    assert.isFalse(failure.message.includes("private SQL"));
    assert.isTrue(closed);
  }
});
