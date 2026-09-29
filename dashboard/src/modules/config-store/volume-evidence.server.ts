import "@tanstack/react-start/server-only";
import { createRequire } from "node:module";
import type * as PloyzSdk from "@ployz/sdk";
import type { ConfigQuery, ConfigView, VolumeObservation } from "@ployz/sdk";
import { Effect, Option, Schema } from "effect";
import { loadOrganizationConnections } from "#/modules/machines/connections.server";

// SAFETY: the package exports this named CommonJS SDK surface at runtime.
const { observeVolumes } = createRequire(import.meta.url)("@ployz/sdk") as Pick<typeof PloyzSdk, "observeVolumes">;

/** The part of a command that decides whether a Deploy can delete Volume data; the Store validates the whole command. */
const Text = Schema.String;
export const AdmitCommand = Schema.Struct({
  command: Schema.Literal("admit"),
  environment: Schema.optional(Schema.Struct({
    project: Schema.optional(Schema.NullOr(Text)),
    environment: Schema.optional(Schema.NullOr(Text)),
  })),
  services: Schema.optional(Schema.Array(Text)),
});

/**
 * For a full Deploy that removes deployed Volumes: which Servers hold their data, as Cloud itself observes them.
 * Nothing here comes from the caller. When the Servers can't be reached the evidence is left out, and the Store refuses
 * the Deploy rather than delete data it could not see.
 */
export const gatherVolumeEvidence = Effect.fn("ConfigStore.gatherVolumeEvidence")(function* (
  organizationId: string,
  command: typeof AdmitCommand.Type | undefined,
  read: (query: ConfigQuery) => Promise<ConfigView>,
) {
  if (command === undefined || (command.services ?? []).length > 0) return undefined;
  const environment = { project: command.environment?.project ?? null, environment: command.environment?.environment ?? null };
  const view = yield* Effect.tryPromise(() => read({ query: "removals", environment })).pipe(Effect.option);
  const removals = Option.getOrUndefined(view);
  if (removals?.view !== "removals" || removals.volumes.length === 0) return undefined;
  const loaded = yield* loadOrganizationConnections(organizationId).pipe(Effect.option);
  const connections = Option.getOrUndefined(loaded);
  if (connections?.kind !== "ready") return undefined;
  const sought = removals.volumes.map((volume) => volume.docker_volume);
  const observed = yield* Effect.tryPromise(() => observeVolumes(connections.connections, sought)).pipe(Effect.option);
  return Option.getOrUndefined(observed) satisfies VolumeObservation | undefined;
});
