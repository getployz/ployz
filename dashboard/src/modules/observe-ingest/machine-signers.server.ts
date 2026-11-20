import "@tanstack/react-start/server-only";
import { createPublicKey, verify } from "node:crypto";
import { and, eq, isNull } from "drizzle-orm";
import { Effect } from "effect";
import type { MachineId } from "#/db/tables";
import { decryptPairingSecret } from "#/modules/machines/connections.server";
import { managementIdentity } from "#/modules/machines/management-identity";
import { organizationMachine } from "#/modules/machines/tables";
import { Database } from "#/server/database.server";
import { Unauthorized } from "#/server/public-error";

/** Whether `signature` is the ed25519 signature of `message` by the management key `identity` (64 hex digits). */
export function verifyManagementSignature(identity: string, message: Uint8Array, signature: Uint8Array) {
  try {
    const x = Buffer.from(identity, "hex").toString("base64url");
    return verify(null, message, createPublicKey({ key: { kty: "OKP", crv: "Ed25519", x }, format: "jwk" }), signature);
  } catch {
    return false;
  }
}

/**
 * The one Organization whose enrolled Machine holds a management key that `signedBy` accepts. A row enrolled before
 * Cloud kept management keys gets its key from its capability on first use. A Machine with no row, or whose key
 * verifies in more than one Organization, is refused.
 */
export const organizationOfSigningMachine = Effect.fn("ObserveIngest.signingOrganization")(function* (
  machineId: MachineId,
  signedBy: (identity: string) => boolean,
) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({
    organizationId: organizationMachine.organizationId,
    managementIdentity: organizationMachine.managementIdentity,
    encryptedCapability: organizationMachine.encryptedCapability,
  }).from(organizationMachine).where(eq(organizationMachine.machineId, machineId));
  const signers: string[] = [];
  for (const row of rows) {
    const identity = row.managementIdentity ?? managementIdentity(yield* decryptPairingSecret(row.encryptedCapability));
    if (identity === null) continue;
    if (row.managementIdentity === null) {
      yield* drizzle.update(organizationMachine).set({ managementIdentity: identity }).where(and(
        eq(organizationMachine.organizationId, row.organizationId),
        eq(organizationMachine.machineId, machineId),
        isNull(organizationMachine.managementIdentity),
      ));
    }
    if (signedBy(identity)) signers.push(row.organizationId);
  }
  const [organizationId, ...others] = signers;
  if (organizationId === undefined) return yield* new Unauthorized();
  if (others.length > 0) {
    // A Machine that left one Organization's Cluster without Cloud seeing it keeps its key, and its row, there.
    yield* Effect.logWarning("A Machine verified for more than one Organization; refused.", { machineId, organizations: signers.length });
    return yield* new Unauthorized();
  }
  return organizationId;
});
