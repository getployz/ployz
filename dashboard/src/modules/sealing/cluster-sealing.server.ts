import "@tanstack/react-start/server-only";
import type { SealingPublicKey } from "@ployz/sdk";
import { Effect } from "effect";
import { currentGeneration } from "#/modules/machines/connections.server";
import { readSealingPublicKey } from "#/modules/machines/sealing-key.server";
import { readSealedVariables } from "#/modules/sealing/sealed-variables.server";

/** Whether a client seals an Organization's Sealed values, and to which key. */
export type ClusterSealing =
  | { readonly state: "off" }
  | { readonly state: "no_server" }
  | { readonly state: "ready"; readonly publicKey: SealingPublicKey };

/**
 * Off while the Organization's `sealedVariables` flag is, which reads nothing else. On, its Cluster's Sealing public key,
 * or no server while it has no current pairing or none of its Servers has reported one.
 */
export const readClusterSealing = Effect.fn("ClusterSealing.read")(function* (organizationId: string) {
  if (!(yield* readSealedVariables(organizationId))) return { state: "off" } as const satisfies ClusterSealing;
  const generation = yield* currentGeneration(organizationId);
  const publicKey = generation === null ? undefined : yield* readSealingPublicKey(organizationId, generation);
  return (publicKey === undefined ? { state: "no_server" } : { state: "ready", publicKey }) satisfies ClusterSealing;
});
