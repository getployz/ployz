import { Effect } from "effect";
import type { PairingChanges } from "#/modules/runtime/organization-runtime.server";

/** The pairing change reader for runtimes whose tests never change the pairing. */
export const noPairingChanges: PairingChanges = {
  current: Effect.succeed("0"),
  changedSince: () => Effect.succeed({ cursor: "0", changed: false }),
};
