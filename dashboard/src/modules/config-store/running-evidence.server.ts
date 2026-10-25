import "@tanstack/react-start/server-only";
import { observeRunning, type RunningObservation } from "@ployz/sdk";
import { Context, Effect, Option, Schema } from "effect";
import { loadOrganizationConnections } from "#/modules/machines/connections.server";
import { storeTry } from "#/modules/config-store/store-sdk.server";
import type { StoreCall } from "./store.contract";

/** Asks Servers which Services they run. The SDK's, unless a test provides another. */
export const RunningObserver = Context.Reference<{ readonly observe: typeof observeRunning }>("ployz/RunningObserver", {
  defaultValue: () => ({ observe: observeRunning }),
});

/** The fields that say whether a Deploy prunes; native Store decoding still validates the original command. */
const DeployCommand = Schema.Struct({
  command: Schema.Literal("admit"),
  admit: Schema.Literal("deploy"),
  services: Schema.optional(Schema.Array(Schema.String)),
  no_prune: Schema.optional(Schema.Boolean),
});

/**
 * For a full Deploy that prunes: which Services the Servers run, as Cloud itself observes them, so the Store names each
 * one the prune removes and refuses a caller who can't destroy. Each Server's answer has its own deadline. Left out when
 * Cloud couldn't try, which the Store reads as not knowing what the prune removes.
 */
export const gatherRunning = Effect.fn("ConfigStore.gatherRunning")(function* (organizationId: string, call: StoreCall) {
  if (call.operation !== "write") return undefined;
  const command = Option.getOrUndefined(Schema.decodeUnknownOption(DeployCommand)(call.command));
  if (command === undefined || (command.services ?? []).length > 0 || command.no_prune === true) return undefined;
  const loaded = yield* loadOrganizationConnections(organizationId).pipe(Effect.option);
  const connections = Option.getOrUndefined(loaded);
  if (connections?.kind !== "ready" || connections.connections.length === 0) return undefined;
  const { observe } = yield* RunningObserver;
  const observed = yield* storeTry(() => observe(connections.connections)).pipe(Effect.option);
  return Option.getOrUndefined(observed) satisfies RunningObservation | undefined;
});
