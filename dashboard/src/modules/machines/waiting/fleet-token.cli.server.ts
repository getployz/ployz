import { Effect, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import { FLEET_TOKEN_LIFETIMES, FleetTokenName } from "#/modules/machines/waiting/fleet-token";
import { createFleetToken, listFleetTokens, revokeFleetToken } from "#/modules/machines/waiting/fleet-token.server";
import { decodeBody, personOnly, type CliRoutes } from "#/modules/commands/cli-call.server";

const NewFleetToken = Schema.Struct({
  name: Schema.optional(FleetTokenName),
  expires_in: Schema.Literals(FLEET_TOKEN_LIFETIMES),
  auto_enroll_max: Schema.optional(Schema.Int.check(Schema.isBetween({ minimum: 1, maximum: 1000 }))),
});

/** Fleet Tokens: what a Machine presents to wait for enrollment. */
export const fleetTokenCliRoutes = {
  "GET fleet-tokens": ({ caller }) =>
    Effect.gen(function* () {
      const tokens = yield* listFleetTokens(caller, { organizationSlug: caller.organization.slug });
      return {
        fleet_tokens: tokens.map((token) => ({
          id: token.id,
          name: token.name,
          expires_at: token.expiresAt,
          expired: token.expired,
          auto_enroll_max: token.autoEnrollMax,
          last_used_at: token.lastUsedAt,
          created_at: token.createdAt,
        })),
      };
    }),
  "POST fleet-tokens": ({ caller, request }) =>
    Effect.gen(function* () {
      const refused = personOnly(caller, "Only a signed-in person can make Fleet Tokens. No changes made.");
      if (refused !== null) return refused;
      const input = yield* decodeBody(NewFleetToken, request,
        "A Fleet Token takes an expiry of 1d, 7d or 30d, and an optional name of lowercase letters, digits and hyphens.");
      const created = yield* createFleetToken(caller, {
        organizationSlug: caller.organization.slug,
        name: input.name ?? null,
        expiresIn: input.expires_in,
        autoEnroll: input.auto_enroll_max === undefined ? null : { maxServers: input.auto_enroll_max },
      });
      return { fleet_token: { id: created.id, name: created.name, token: created.token, expires_at: created.expiresAt } };
    }),
  "DELETE fleet-tokens/:id": ({ caller, id }) =>
    Effect.gen(function* () {
      const refused = personOnly(caller, "Only a signed-in person can revoke Fleet Tokens; revoke it under Servers → Tokens. No changes made.");
      if (refused !== null) return refused;
      const ref = decodeURIComponent(id ?? "");
      const target = Schema.is(Uuid)(ref) ? { id: ref } : { name: ref };
      const revoked = yield* revokeFleetToken(caller, { organizationSlug: caller.organization.slug, ...target });
      return { waiting_removed: revoked.waitingRemoved };
    }),
} satisfies CliRoutes;
