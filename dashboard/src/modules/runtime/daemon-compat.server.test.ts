import { it } from "@effect/vitest";
import { Effect } from "effect";
import { expect } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { serversLackingEnsure } from "#/modules/runtime/daemon-compat.server";
import { type ConnectedRuntimeClient, OrganizationRuntime, type OrganizationRuntimeService } from "#/modules/runtime/organization-runtime.server";

const runtime = (machines: ReadonlyArray<{ readonly name: string; readonly capabilities?: ReadonlyArray<string> }>) =>
  asTestDouble<OrganizationRuntimeService>()({
    cancel: () => Effect.void,
    open: () => Effect.succeed({ status: "connected" as const, connected: asTestDouble<ConnectedRuntimeClient>()({
      watchFirstFrame: () => Effect.succeed({ machines: machines.map(({ name, capabilities }) => ({ machine: { name, runtime: { capabilities } } })) }),
    }) }),
  });

it.effect("names the Servers that can't ensure Containers, and attempts the ones it can't read", () => Effect.gen(function* () {
  const lacking = yield* serversLackingEnsure("org").pipe(Effect.provideService(OrganizationRuntime, runtime([
    { name: "alpha", capabilities: ["ployz.container.ensure.v1"] },
    { name: "beta", capabilities: ["ployz.volume.v1"] },
    { name: "gamma" },
  ])));
  expect(lacking).toEqual(["beta"]);
}));

it.effect("names none when no Server is connected", () => Effect.gen(function* () {
  const none = asTestDouble<OrganizationRuntimeService>()({ cancel: () => Effect.void, open: () => Effect.succeed({ status: "no_connection" as const }) });
  expect(yield* serversLackingEnsure("org").pipe(Effect.provideService(OrganizationRuntime, none))).toEqual([]);
}));
