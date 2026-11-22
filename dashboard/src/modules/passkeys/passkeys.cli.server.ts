import { Effect } from "effect";
import { readPasskeyRequest } from "#/modules/passkeys/passkeys.server";
import { missingRefusal, shellPersonOnly, type CliRoutes } from "#/server/cli-call";

/** A passkey request a Device waits on. */
export const passkeysCliRoutes = {
  "GET passkey-requests/:id": ({ caller, id }) =>
    Effect.gen(function* () {
      const refused = shellPersonOnly(caller, "Only a signed-in person can read a passkey request.");
      if (refused !== null) return refused;
      const pending = yield* readPasskeyRequest(caller.organization.id, decodeURIComponent(id ?? "").trim().toLowerCase());
      if (pending === null) return missingRefusal("No such passkey request.");
      return pending;
    }),
} satisfies CliRoutes;
