import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { Conflict } from "#/server/public-error";

/**
 * Whether a founding Server's ployz takes part in Sealing Key recovery, which it declares with `recoverySupport`.
 * Mixed-version handling only: delete it once every Server runs a ployz that declares it.
 */
export const supportsSealingRecovery = Effect.fn("SealingRecoveryCompat.supports")(function* (
  recoverySupport: 1 | undefined, organizationHasSealingKey: boolean,
) {
  if (recoverySupport === 1) return true;
  if (!organizationHasSealingKey) return false;
  return yield* new Conflict({ message: "Update ployz on this server to restore its Cluster's Sealing Key.", userFacing: true });
});
