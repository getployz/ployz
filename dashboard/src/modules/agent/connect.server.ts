import "@tanstack/react-start/server-only";
import { and, eq, isNull, lt, ne, or } from "drizzle-orm";
import { Effect } from "effect";
import { oauthClient, oauthConsent } from "#/modules/identity/oauth-tables";
import { member } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { Auth, type RemoteMcpAuth } from "#/server/auth.server";
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

/**
 * The agent asking: its registered name, if it gave one, and the host its code goes to, which it can't fake. Null when
 * better-auth didn't sign the query, it expired, or the agent is gone.
 */
const askingAgent = Effect.fn("Connect.askingAgent")(function* (mcp: RemoteMcpAuth, pageQuery: string) {
  const oauthQuery = signedOAuthQuery(pageQuery);
  if (!(yield* mcp.signed(oauthQuery))) return null;
  const params = new URLSearchParams(oauthQuery);
  const redirect = URL.parse(params.get("redirect_uri") ?? "");
  if (redirect === null) return null;
  const database = yield* Database;
  const [client] = yield* database.drizzle
    .select({ name: oauthClient.name })
    .from(oauthClient)
    .where(and(eq(oauthClient.clientId, params.get("client_id") ?? ""), or(isNull(oauthClient.disabled), ne(oauthClient.disabled, true))))
    .limit(1);
  return client === undefined ? null : { name: client.name, host: redirect.host };
});

/** Who is connecting, the Organizations they may pick, and the agent the page's signed query names; null when signed out. */
export const connectChoices = Effect.fn("Connect.choices")(function* (headers: Headers, pageQuery: string) {
  const { auth, mcp } = yield* hostedMcp;
  const signedIn = yield* auth.getSession(headers);
  if (signedIn === null) return null;
  const [organizations, agent] = yield* Effect.all([memberships(signedIn.user.id), askingAgent(mcp, pageQuery)], { concurrency: 2 });
  return { email: signedIn.user.email, organizations, agent };
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
 * to; their session's active Organization is left alone. Allow first deletes their consents there that predate this
 * membership, which can never authorize a call again, so better-auth writes a fresh grant instead of keeping a dead one.
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
    .select({ organizationId: member.organizationId, joinedAt: member.createdAt })
    .from(member)
    .where(and(eq(member.userId, signedIn.user.id), eq(member.organizationId, input.organizationId)))
    .limit(1);
  if (membership === undefined) return yield* new Forbidden({ message: "You are not a member of that Organization." });
  if (input.accept) {
    // Truncated to the second as the caller check compares, so a consent made in the joining second survives.
    const joinedSecond = new Date(Math.floor(membership.joinedAt.getTime() / 1000) * 1000);
    yield* database.drizzle.delete(oauthConsent).where(and(
      eq(oauthConsent.userId, signedIn.user.id),
      eq(oauthConsent.referenceId, membership.organizationId),
      lt(oauthConsent.createdAt, joinedSecond),
    ));
  }
  return yield* mcp.consent(
    headers,
    { oauthQuery: signedOAuthQuery(input.oauthQuery), accept: input.accept },
    { userId: signedIn.user.id, sessionId: signedIn.session.id, organizationId: membership.organizationId },
  );
});
