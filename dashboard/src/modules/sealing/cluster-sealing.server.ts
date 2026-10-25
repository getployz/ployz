import "@tanstack/react-start/server-only";
import type { SealingPublicKey } from "@ployz/sdk";
import { Effect } from "effect";
import { currentGeneration } from "#/modules/machines/connections.server";
import { readSealingPublicKey } from "#/modules/machines/sealing-key.server";

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

export const readClusterSealingToSeal = readClusterSealing;
