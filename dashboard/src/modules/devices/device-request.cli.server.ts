import { Effect, Schema } from "effect";
import { DEVICE_KEY } from "#/modules/devices/device-code";
import { readDeviceRequest, requestDevice } from "#/modules/devices/device-request.server";
import { decodeBody, missingRefusal, personOnly, shellPersonOnly, type CliRoutes } from "#/server/cli-call";
import { cliRefusalResponse } from "#/server/public-error";

const NewDeviceRequest = Schema.Struct({ key: Schema.String.check(Schema.isPattern(DEVICE_KEY)) });

/** A Device asking to join: the code it shows, and the key it holds. */
export const deviceRequestCliRoutes = {
  "POST device-requests": ({ caller, request }) =>
    Effect.gen(function* () {
      const refused = personOnly(caller, "Only a signed-in person can request a Device. No changes made.");
      if (refused !== null) return refused;
      const { key } = yield* decodeBody(NewDeviceRequest, request, "A Device request takes the Device's key as 64 lowercase hex digits.");
      const requested = yield* requestDevice(caller.organization.id, key);
      if (requested === null) {
        return cliRefusalResponse({ code: "device_request_conflict", message: "Another Device's request holds this code. No changes made.", details: null });
      }
      return { code: requested.code, expires_at: requested.expiresAt.toISOString() };
    }),
  "GET device-requests/:id": ({ caller, id }) =>
    Effect.gen(function* () {
      const refused = shellPersonOnly(caller, "Only a signed-in person can read a Device request.");
      if (refused !== null) return refused;
      const pending = yield* readDeviceRequest(caller.organization.id, decodeURIComponent(id ?? "").trim().toLowerCase());
      if (pending === null) return missingRefusal("No such Device request.");
      return { code: pending.code, key: pending.key, expires_at: pending.expiresAt.toISOString() };
    }),
} satisfies CliRoutes;
