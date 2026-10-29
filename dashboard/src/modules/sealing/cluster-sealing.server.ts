import "@tanstack/react-start/server-only";
import type { SealingPublicKey } from "@ployz/sdk";
import { Effect } from "effect";
import { currentGeneration } from "#/modules/machines/connections.server";
import { readSealingPublicKey } from "#/modules/machines/sealing-key-read.server";
import { recordSealingPublicKey } from "#/modules/machines/sealing-key.server";
import { firstRuntimeFrame } from "#/modules/runtime/organization-runtime.server";

/** The key a client seals an Organization's Sealed values to, once its Cluster has one. */
export type ClusterSealing =
  | { readonly state: "no_server"; readonly awaitsRecovery?: true }
  | { readonly state: "ready"; readonly publicKey: SealingPublicKey };

/** The Cluster's Sealing public key, or no server while it has no current pairing or none of its Servers has reported one. */
export const readClusterSealing = Effect.fn("ClusterSealing.read")(function* (organizationId: string) {
  const generation = yield* currentGeneration(organizationId);
  const publicKey = generation === null ? undefined : yield* readSealingPublicKey(organizationId, generation);
  return (publicKey === undefined ? { state: "no_server" } : { state: "ready", publicKey }) satisfies ClusterSealing;
});

/**
 * {@link readClusterSealing}, for a client about to seal. A Server that got the key back through `ployz recover`
 * reported none when it enrolled, so with no key recorded Cloud asks the Cluster and records the one its Servers hold.
 */
export const readClusterSealingToSeal = Effect.fn("ClusterSealing.readToSeal")(function* (organizationId: string) {
  const recorded = yield* readClusterSealing(organizationId);
  const generation = yield* currentGeneration(organizationId);
  if (recorded.state === "ready" || generation === null) return recorded;
  const frame = yield* firstRuntimeFrame(organizationId).pipe(Effect.orElseSucceed(() => null));
  const sealings = frame?.machines.map(({ machine: { runtime } }) => runtime.sealing) ?? [];
  const held = new Set(sealings.flatMap((sealing) => sealing?.kind === "held" ? [sealing.public_key] : []));
  const [publicKey] = held;
  // Until `ployz recover` runs, the client is told that, not to add a Server, which couldn't get the key either.
  if (held.size === 0 && sealings.some((sealing) => sealing?.kind === "awaits_recovery")) {
    return { state: "no_server", awaitsRecovery: true } satisfies ClusterSealing;
  }
  // A pairing replaced during the read must not take the old Cluster's key.
  if (held.size !== 1 || publicKey === undefined || (yield* currentGeneration(organizationId)) !== generation) return recorded;
  // The reseal request is queued durably, so a seal never waits on Inngest to take it.
  yield* recordSealingPublicKey(organizationId, generation, publicKey, "detach");
  return yield* readClusterSealing(organizationId);
});
