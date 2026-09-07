import type { Client, ConnectOptions } from "@ployz/sdk";
import { assert, it } from "@effect/vitest";
import { Effect } from "effect";
import { asTestDouble } from "#/lib/test-double";
import {
  makePloyzLayer,
  Ployz,
  PloyzProviderError,
} from "#/modules/runtime/ployz.server";

const options = asTestDouble<ConnectOptions>()({
  relayUrl: "wss://relay.example.test",
  bearer: "tenant-token",
  pairing: "ppair_test",
  machineId: "machine-a",
});

it.effect("scopes each connected Ployz session", () =>
  Effect.gen(function* () {
    let opened = 0;
    let closed = 0;
    const layer = makePloyzLayer({
      connect: async () => {
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
        assert.strictEqual(closed, 0);
      }),
    ).pipe(Effect.provide(layer));

    assert.strictEqual(closed, 1);
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
