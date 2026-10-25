import "@tanstack/react-start/server-only";
import type { SealingPublicKey } from "@ployz/sdk";
import { Effect } from "effect";
import { currentGeneration } from "#/modules/machines/connections.server";
import { readSealingPublicKey, recordSealingPublicKey } from "#/modules/machines/sealing-key.server";
import { firstRuntimeFrame } from "#/modules/runtime/organization-runtime.server";

/** The key a client seals an Organization's Sealed values to, once its Cluster has one. */
export type ClusterSealing =
  | { readonly state: "no_server" }
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
  const held = new Set(frame?.machines.flatMap(({ machine: { runtime } }) =>
    runtime.sealing?.kind === "held" ? [runtime.sealing.public_key] : []));
  const [publicKey] = held;
  // A pairing replaced during the read must not take the old Cluster's key.
  if (held.size !== 1 || publicKey === undefined || (yield* currentGeneration(organizationId)) !== generation) return recorded;
  yield* recordSealingPublicKey(organizationId, generation, publicKey);
  return yield* readClusterSealing(organizationId);
});
