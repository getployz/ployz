import "@tanstack/react-start/server-only";
import { createRequire } from "node:module";
import type * as PloyzSdk from "@ployz/sdk";
import type { VolumeObservation } from "@ployz/sdk";
import { Effect, Option, Schema } from "effect";
import { loadOrganizationConnections } from "#/modules/machines/connections.server";
import { storeTry } from "#/modules/config-store/store-sdk.server";
import { EnvironmentRef, environmentOf, type StoreCall, type StoreRead } from "./store.contract";

// SAFETY: the package exports this named CommonJS SDK surface at runtime.
const { observeVolumes } = createRequire(import.meta.url)("@ployz/sdk") as Pick<typeof PloyzSdk, "observeVolumes">;

/** The fields used to gather evidence; native Store decoding still validates the original command. */
const AdmitCommand = Schema.Struct({
  command: Schema.Literal("admit"),
  // A retry needs no evidence: it ships the Volume identities its source accepted.
  admit: Schema.Literals(["deploy", "remove"]),
  environment: Schema.optional(EnvironmentRef),
  services: Schema.optional(Schema.Array(Schema.String)),
});

const EvidenceCommand = Schema.Union([AdmitCommand, Schema.Struct({
  command: Schema.Literal("publish"),
  environment: Schema.optional(EnvironmentRef),
})]);

/**
 * For a Publish, full Deploy, or removal, that removes deployed Volumes: which Servers hold their data, as Cloud itself observes them.
 * Nothing here comes from the caller. When the Servers can't be reached the evidence is left out, and the Store refuses
 * the write rather than act on data it could not see.
 */
export const gatherVolumeEvidence = Effect.fn("ConfigStore.gatherVolumeEvidence")(function* (
  organizationId: string,
  call: StoreCall,
  read: StoreRead,
) {
  if (call.operation !== "write") return undefined;
  const command = Option.getOrUndefined(Schema.decodeUnknownOption(EvidenceCommand)(call.command));
  if (command === undefined || (command.command === "admit" && (command.services ?? []).length > 0)) return undefined;
  const environment = environmentOf(command.environment);
  const view = yield* storeTry(() => read({ query: "removals", environment, remove: command.command === "admit" && command.admit === "remove" })).pipe(Effect.option);
  const removals = Option.getOrUndefined(view);
  if (removals === undefined || removals.volumes.length === 0) return undefined;
  const loaded = yield* loadOrganizationConnections(organizationId).pipe(Effect.option);
  const connections = Option.getOrUndefined(loaded);
  if (connections?.kind !== "ready") return undefined;
  const sought = removals.volumes.map((volume) => volume.docker_volume);
  const observed = yield* storeTry(() => observeVolumes(connections.connections, sought)).pipe(Effect.option);
  return Option.getOrUndefined(observed) satisfies VolumeObservation | undefined;
});
