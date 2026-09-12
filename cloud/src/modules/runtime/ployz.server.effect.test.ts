import type { Client, Connection } from "@ployz/sdk";
import { assert, it } from "@effect/vitest";
import { Deferred, Effect, Fiber } from "effect";
import { asTestDouble } from "#/lib/test-double";
import { MissingDataLossIdentities } from "#/modules/runtime/data-loss-confirm";
import {
  makePloyzLayer,
  Ployz,
  PloyzProviderError,
} from "#/modules/runtime/ployz.server";

const options = {
  connections: [{ tailcat: "tailcat://candidate" }] satisfies Connection[],
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
