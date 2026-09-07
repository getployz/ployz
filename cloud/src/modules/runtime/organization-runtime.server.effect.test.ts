import type { Client } from "@ployz/sdk";
import { assert, it } from "@effect/vitest";
import { Effect, Layer } from "effect";
import { asTestDouble } from "#/lib/test-double";
import type { DialTenant } from "#/modules/runtime/dial-entry";
import {
  makeOrganizationRuntimeLayer,
  OrganizationRuntime,
} from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";

const tenant = {
  relayUrl: "wss://relay.example.test",
  bearer: "tenant-token",
  pairing: "ppair_test",
  preferredMachineId: "preferred",
  enrolledMachineIds: ["preferred", "spare"],
} satisfies DialTenant;

it.effect("fails over and finalizes the connected organization session", () =>
  Effect.gen(function* () {
    const dialed: string[] = [];
    let closed = 0;
    const ployz = makePloyzLayer({
      connect: async (options) => {
        dialed.push(options.machineId);
        if (options.machineId === "preferred") throw new Error("offline");
        return asTestDouble<Client>()({
          close: async () => {
            closed += 1;
          },
        });
      },
    });
    const runtime = makeOrganizationRuntimeLayer(() =>
      Effect.succeed({ kind: "ready", tenant }),
    ).pipe(Layer.provide(ployz));

    yield* Effect.scoped(
      Effect.gen(function* () {
        const organizationRuntime = yield* OrganizationRuntime;
        const session = yield* organizationRuntime.open("org-1");
        assert.strictEqual(session.status, "connected");
        assert.deepStrictEqual(dialed, ["preferred", "spare"]);
        assert.strictEqual(closed, 0);
      }),
    ).pipe(Effect.provide(runtime));

    assert.strictEqual(closed, 1);
  }),
);

it.effect("keeps missing pairing distinct from unreachable Relay", () =>
  Effect.gen(function* () {
    const ployz = makePloyzLayer({
      connect: async () => {
        throw new Error("must not dial");
      },
    });
    const missing = makeOrganizationRuntimeLayer(() =>
      Effect.succeed({ kind: "missing" }),
    ).pipe(Layer.provide(ployz));
    const unreachable = makeOrganizationRuntimeLayer(() =>
      Effect.succeed({ kind: "unreachable", cause: "empty", error: null }),
    ).pipe(Layer.provide(ployz));
    const open = Effect.scoped(
      Effect.flatMap(OrganizationRuntime, (runtime) => runtime.open("org-1")),
    );

    assert.deepStrictEqual(
      yield* open.pipe(Effect.provide(missing)),
      { status: "no_connection" },
    );
    assert.deepStrictEqual(
      yield* open.pipe(Effect.provide(unreachable)),
      { status: "unreachable", error: null },
    );
  }),
);
