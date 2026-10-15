import "@tanstack/react-start/server-only";
import { observeVolumes, type VolumeObservation } from "@ployz/sdk";
import { Effect, Option, Schema } from "effect";
import { loadOrganizationConnections } from "#/modules/machines/connections.server";
import { storeTry } from "#/modules/config-store/store-sdk.server";
import { EnvironmentRef, environmentOf, type StoreCall, type StoreRead } from "./store.contract";

/** The fields used to gather evidence; native Store decoding still validates the original command. */
const AdmitCommand = Schema.Struct({
  command: Schema.Literal("admit"),
  // A retry needs no evidence: it ships the Volume identities its source accepted.
  admit: Schema.Literals(["deploy", "remove", "shut_down"]),
  environment: Schema.optional(EnvironmentRef),
  services: Schema.optional(Schema.Array(Schema.String)),
});

const RemoveCommand = Schema.Struct({
  command: Schema.Literal("remove"),
  project: Schema.String,
  environment: Schema.optional(Schema.NullOr(Schema.String)),
});

const EvidenceCommand = Schema.Union([AdmitCommand, RemoveCommand, Schema.Struct({
  command: Schema.Literal("publish"),
  environment: Schema.optional(EnvironmentRef),
})]);

/**
 * For a Save, full Deploy, or removal, that removes deployed Volumes: which Servers hold their data, as Cloud itself observes them.
 * A Project's removal asks about every Environment's. Nothing here comes from the caller. When the Servers can't be
 * reached the evidence is left out, and the Store refuses the write rather than act on data it could not see.
 */
export const gatherVolumeEvidence = Effect.fn("ConfigStore.gatherVolumeEvidence")(function* (
  organizationId: string,
  call: StoreCall,
  read: StoreRead,
) {
  if (call.operation !== "write") return undefined;
  const command = Option.getOrUndefined(Schema.decodeUnknownOption(EvidenceCommand)(call.command));
  if (command === undefined || (command.command === "admit" && (command.services ?? []).length > 0)) return undefined;
  const asked = yield* Effect.option(questions(command, read));
  const sought = Option.getOrUndefined(asked)?.flatMap((removals) => removals.volumes.map((volume) => volume.docker_volume));
  if (sought === undefined || sought.length === 0) return undefined;
  const loaded = yield* loadOrganizationConnections(organizationId).pipe(Effect.option);
  const connections = Option.getOrUndefined(loaded);
  if (connections?.kind !== "ready") return undefined;
  const observed = yield* storeTry(() => observeVolumes(connections.connections, sought)).pipe(Effect.option);
  return Option.getOrUndefined(observed) satisfies VolumeObservation | undefined;
});

/**
 * For a Git push: which Servers hold the Volumes whose loss was accepted when they were saved. Without it, or when the
 * Servers can't be reached, the push keeps their data and the next Deploy asks again.
 */
export const systemVolumeEvidence = Effect.fn("ConfigStore.systemVolumeEvidence")(function* (
  organizationId: string,
  read: StoreRead,
) {
  const consented = yield* storeTry(() => read({ query: "consented" })).pipe(Effect.option);
  const sought = Option.getOrUndefined(consented)?.volumes;
  if (sought === undefined || sought.length === 0) return undefined;
  const loaded = yield* loadOrganizationConnections(organizationId).pipe(Effect.option);
  const connections = Option.getOrUndefined(loaded);
  if (connections?.kind !== "ready") return undefined;
  const observed = yield* storeTry(() => observeVolumes(connections.connections, sought)).pipe(Effect.option);
  return Option.getOrUndefined(observed) satisfies VolumeObservation | undefined;
});

/** The Volumes `command` may remove, per Environment it touches. */
const questions = Effect.fn("ConfigStore.volumeQuestions")(function* (command: typeof EvidenceCommand.Type, read: StoreRead) {
  if (command.command !== "remove") {
    const remove = command.command === "admit" && command.admit !== "deploy";
    return [yield* storeTry(() => read({ query: "removals", environment: environmentOf(command.environment), remove }))];
  }
  const { project } = command;
  const members = command.environment != null ? [command.environment]
    : (yield* storeTry(() => read({ query: "environments", project }))).environments.map((listing) => listing.name);
  return yield* Effect.forEach(members, (environment) =>
    storeTry(() => read({ query: "removals", environment: { project, environment }, remove: true })));
});
