import "@tanstack/react-start/server-only";
import { observeVolumes, type VolumeObservation } from "@ployz/sdk";
import { Context, Effect, Option, Schema } from "effect";
import { loadOrganizationConnections } from "#/modules/machines/connections.server";
import { storeTry } from "#/modules/config-store/store-sdk.server";
import { EnvironmentRef, environmentOf, type StoreCall, type StoreRead } from "./store.contract";

/** Asks Servers which of the `sought` Docker Volumes they hold. The SDK's, unless a test provides another. */
export const VolumeObserver = Context.Reference<{ readonly observe: typeof observeVolumes }>("ployz/VolumeObserver", {
  defaultValue: () => ({ observe: observeVolumes }),
});

/** The fields used to gather evidence; native Store decoding still validates the original command. */
const AdmitCommand = Schema.Struct({
  command: Schema.Literal("admit"),
  admit: Schema.Literals(["deploy", "remove", "shut_down"]),
  environment: Schema.optional(EnvironmentRef),
});

/** A plain retry ships the Volume identities its source accepted; a rollback reviews what it deletes now. */
const RetryCommand = Schema.Struct({
  command: Schema.Literal("admit"),
  admit: Schema.Literal("retry"),
  deployment: Schema.String,
});

const RemoveCommand = Schema.Struct({
  command: Schema.Literal("remove"),
  project: Schema.String,
  environment: Schema.optional(Schema.NullOr(Schema.String)),
});

const EvidenceCommand = Schema.Union([AdmitCommand, RetryCommand, RemoveCommand, Schema.Struct({
  command: Schema.Literal("publish"),
  environment: Schema.optional(EnvironmentRef),
})]);

/**
 * For a Save, Deploy, or removal, that removes deployed Volumes: which Servers hold their data, as Cloud itself observes them.
 * A Project's removal asks about every Environment's. Nothing here comes from the caller. When the Servers can't be
 * reached the evidence names them and why, or is left out when Cloud couldn't try, and the Store refuses the write
 * rather than act on data it could not see.
 */
export const gatherVolumeEvidence = Effect.fn("ConfigStore.gatherVolumeEvidence")(function* (
  organizationId: string,
  call: StoreCall,
  read: StoreRead,
) {
  if (call.operation !== "write") return undefined;
  const command = Option.getOrUndefined(Schema.decodeUnknownOption(EvidenceCommand)(call.command));
  // A Deploy of named Services still saves Working State, so it reviews the Volumes that Save drops as a full one does.
  if (command === undefined) return undefined;
  const asked = yield* Effect.option(questions(command, read));
  const sought = Option.getOrUndefined(asked)?.flatMap((removals) => removals.volumes.map((volume) => volume.docker_volume));
  if (sought === undefined || sought.length === 0) return undefined;
  const loaded = yield* loadOrganizationConnections(organizationId).pipe(Effect.option);
  const connections = Option.getOrUndefined(loaded);
  if (connections?.kind !== "ready") return undefined;
  const { observe } = yield* VolumeObserver;
  const observed = yield* storeTry(() => observe(connections.connections, sought)).pipe(Effect.option);
  return Option.getOrUndefined(observed) satisfies VolumeObservation | undefined;
});

/**
 * For a removal's admission: which Servers didn't answer Cloud just now, every one when none could be reached. A removal
 * that failed goes again once one of them answers, Volumes or not. `volumes` already names them when it was gathered.
 * Undefined when Cloud couldn't look, such as when its connections didn't load: the Store then keeps waiting on the
 * Servers that were silent before, never reading it as all of them answering.
 */
export const gatherUnanswered = Effect.fn("ConfigStore.gatherUnanswered")(function* (
  organizationId: string,
  call: StoreCall,
  volumes: VolumeObservation | undefined,
) {
  if (call.operation !== "write") return undefined;
  // Decoded before any Server is asked: a malformed removal is the Store's to refuse, not worth a probe.
  const command = Option.getOrUndefined(Schema.decodeUnknownOption(EvidenceCommand)(call.command));
  const removes = command?.command === "remove" || (command?.command === "admit" && command.admit === "remove");
  if (!removes) return undefined;
  if (volumes !== undefined) return volumes.unanswered;
  const loaded = yield* loadOrganizationConnections(organizationId).pipe(Effect.option);
  const connections = Option.getOrUndefined(loaded);
  if (connections?.kind !== "ready") return undefined;
  if (connections.connections.length === 0) return [];
  const { observe } = yield* VolumeObserver;
  const observed = yield* storeTry(() => observe(connections.connections, [])).pipe(Effect.option);
  return Option.match(observed, {
    onNone: () => connections.connections.flatMap((connection) => connection.machine_id ?? []),
    onSome: (observation) => observation.unanswered,
  });
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
  const { observe } = yield* VolumeObserver;
  const observed = yield* storeTry(() => observe(connections.connections, sought)).pipe(Effect.option);
  return Option.getOrUndefined(observed) satisfies VolumeObservation | undefined;
});

/** The Volumes `command` may remove, per Environment it touches. */
const questions = Effect.fn("ConfigStore.volumeQuestions")(function* (command: typeof EvidenceCommand.Type, read: StoreRead) {
  if ("deployment" in command) {
    return [yield* storeTry(() => read({ query: "removals", environment: environmentOf(undefined), remove: false, deployment: command.deployment }))];
  }
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
