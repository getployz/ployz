import { Effect, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import { TOKEN_KINDS } from "#/modules/identity/actor";
import { requireClaimed } from "#/modules/identity/claim-required.server";
import { createOrganizationToken, knownEnvironment, listCredentials, revokeCredential } from "#/modules/identity/organization-token.server";
import { TOKEN_PRESETS, presetOf } from "#/modules/identity/token-grant";
import { pendingServerRevocations, retireServerAccess } from "#/modules/machines/server-access.server";
import { decodeBody, personOnly, type CliRoutes } from "#/server/cli-call";
import { cliRefusalResponse, NotFound, Validation } from "#/server/public-error";

const NewToken = Schema.Struct({
  name: Schema.Trim.check(Schema.isMinLength(1), Schema.isMaxLength(64)),
  expires_in_days: Schema.Int.check(Schema.isBetween({ minimum: 1, maximum: 365 })),
  kind: Schema.optional(Schema.Literals(TOKEN_KINDS)),
  preset: Schema.optional(Schema.Literals(TOKEN_PRESETS)),
  environment: Schema.optional(Schema.String.check(Schema.isMaxLength(253))),
});

const NewTokenName = Schema.Struct({ name: NewToken.fields.name });

/** Rerunning `token rm` on a credential already gone retries the Clears its Servers haven't confirmed. */
const pendingRevocation = Effect.fn("Cli.pendingRevocation")(function* (organizationId: string, id: string, missing: NotFound) {
  const pending = (yield* pendingServerRevocations(organizationId)).find((entry) => entry.id === id);
  if (pending === undefined) return yield* missing;
  return { id, kind: pending.kind };
});

/** Organization Tokens, and the signed-in devices that act as one Caller. */
export const tokensCliRoutes = {
  "GET tokens": ({ caller }) =>
    Effect.gen(function* () {
      return {
        ...(yield* listCredentials(caller)),
        revoking: yield* pendingServerRevocations(caller.organization.id),
      };
    }),
  "POST tokens": ({ caller, request }) =>
    Effect.gen(function* () {
      if (caller.credential.kind !== "session") {
        return yield* requireClaimed(caller.organization.id, "mint_token").pipe(
          Effect.as(personOnly(caller, "Only a signed-in person can make tokens. No changes made.")),
          Effect.catchTag("StoreRefused", (refused) => Effect.succeed(cliRefusalResponse(refused.refusal))),
        );
      }
      // The CLI checks the rest before it asks, so a bad name is the one a person can send; it is named alone.
      yield* decodeBody(NewTokenName, request.clone(), "A token's name is 1 to 64 characters. No changes made.");
      const input = yield* decodeBody(NewToken, request,
        "A token expires in 1 to 365 days, and its kind is cli or agent. No changes made.");
      const chosen = presetOf(input);
      if (!chosen.ok) return yield* new Validation({ message: chosen.message, userFacing: true });
      const preset = yield* knownEnvironment(caller, chosen.preset);
      return yield* createOrganizationToken(caller, { name: input.name, expiresInDays: input.expires_in_days, kind: input.kind, preset }).pipe(
        Effect.map((token) => ({ token })),
        Effect.catchTag("StoreRefused", (refused) => Effect.succeed(cliRefusalResponse(refused.refusal))),
      );
    }),
  "DELETE tokens/:id": ({ caller, id }) =>
    Effect.gen(function* () {
      const refused = personOnly(caller, "Only a signed-in person can revoke tokens; revoke it under Organization → Tokens. No changes made.");
      if (refused !== null) return refused;
      if (!Schema.is(Uuid)(id)) return yield* new NotFound({ message: "No such token or signed-in device." });
      const removed = yield* revokeCredential(caller, id).pipe(
        Effect.catchTag("NotFound", (missing) => pendingRevocation(caller.organization.id, id, missing)),
      );
      return { removed, servers: yield* retireServerAccess(id) };
    }),
  "POST logout": ({ caller }) =>
    Effect.gen(function* () {
      if (caller.credential.kind !== "session") {
        return yield* new Validation({ message: "An Organization Token isn't signed in; revoke it with `ployz token rm`.", userFacing: true });
      }
      yield* revokeCredential(caller, caller.credential.id);
      return { signed_out: { id: caller.credential.id }, servers: yield* retireServerAccess(caller.credential.id) };
    }),
} satisfies CliRoutes;
