import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { member } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { Auth } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { Forbidden, NotFound, Unauthorized } from "#/server/public-error";

const memberships = Effect.fn("Connect.memberships")(function* (userId: string) {
  const database = yield* Database;
  return yield* database.drizzle
    .select({ id: organization.id, slug: organization.slug, name: organization.name })
    .from(member)
    .innerJoin(organization, eq(organization.id, member.organizationId))
    .where(eq(member.userId, userId))
    .orderBy(organization.slug);
});

const hostedMcp = Effect.gen(function* () {
  const auth = yield* Auth;
  if (auth.mcp === null) return yield* new NotFound({ message: "Remote agents connect only to Ployz Cloud." });
  return { auth, mcp: auth.mcp };
});

/** Whether this Cloud takes remote agents. A Self-hosted Cloud answers every request to /connect with a 404. */
export const remoteAgentsEnabled = Effect.gen(function* () {
  const auth = yield* Auth;
  return auth.mcp !== null;
});

export const connectChoices = Effect.fn("Connect.choices")(function* (headers: Headers) {
  const { auth } = yield* hostedMcp;
  const signedIn = yield* auth.getSession(headers);
  if (signedIn === null) return null;
  return { email: signedIn.user.email, organizations: yield* memberships(signedIn.user.id) };
});

/**
 * The part of the /connect page's query that better-auth signed: `sig`, the `ba_param` list, and the parameters it
 * names. Anything else the browser picked up would break the signature. Mirrors `buildSignedOAuthQuery` in
 * @better-auth/oauth-provider's client, which the package doesn't export.
 */
const signedOAuthQuery = (pageQuery: string) => {
  const params = new URLSearchParams(pageQuery);
  const named = new Set(params.getAll("ba_param"));
  if (!params.has("sig") || named.size === 0) return pageQuery;
  const signed = new URLSearchParams();
  for (const [key, value] of params) if (key === "sig" || key === "ba_param" || named.has(key)) signed.append(key, value);
  return signed.toString();
};

/**
 * The member's answer on /connect. The grant is bound to the Organization they picked, which must be one they belong
 * to; their session's active Organization is left alone.
 */
export const connectAgent = Effect.fn("Connect.decide")(function* (
  headers: Headers,
  input: { readonly organizationId: string; readonly accept: boolean; readonly oauthQuery: string },
) {
  const { auth, mcp } = yield* hostedMcp;
  const signedIn = yield* auth.getSession(headers);
  if (signedIn === null) return yield* new Unauthorized();
  const database = yield* Database;
  const [membership] = yield* database.drizzle
    .select({ organizationId: member.organizationId })
    .from(member)
    .where(and(eq(member.userId, signedIn.user.id), eq(member.organizationId, input.organizationId)))
    .limit(1);
  if (membership === undefined) return yield* new Forbidden({ message: "You are not a member of that Organization." });
  return yield* mcp.consent(
    headers,
    { oauthQuery: signedOAuthQuery(input.oauthQuery), accept: input.accept },
    { userId: signedIn.user.id, sessionId: signedIn.session.id, organizationId: membership.organizationId },
  );
});
