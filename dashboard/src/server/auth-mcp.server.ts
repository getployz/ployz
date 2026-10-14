import { AsyncLocalStorage } from "node:async_hooks";
import { createHash } from "node:crypto";
import { cimd } from "@better-auth/cimd";
import { fetchClientMetadataResource } from "@better-auth/cimd/node";
import { mcp } from "@better-auth/mcp";
import type { OAuthClaimExtensionInput } from "@better-auth/oauth-provider";
import type { BetterAuthPlugin } from "better-auth";
import { APIError, createAuthMiddleware } from "better-auth/api";
import { jwt } from "better-auth/plugins";
import { Option, Schema } from "effect";

export const MCP_SCOPES = ["ployz", "offline_access"] as const;
export const CONNECT_PAGE = "/connect";

export const mcpEndpoints = (appUrl: URL) => ({
  issuer: new URL("/api/auth", appUrl).href,
  resource: new URL("/mcp", appUrl).href,
  jwks: new URL("/api/auth/jwks", appUrl).href,
});

export type ConsentChoice = { readonly userId: string; readonly sessionId: string; readonly organizationId: string };
const consentChoice = new AsyncLocalStorage<ConsentChoice>();
export const withConsentChoice = <A>(choice: ConsentChoice, run: () => Promise<A>) => consentChoice.run(choice, run);
const chosenOrganization = ({ user, session }: { user: { id: string }; session: { id: string } }) => {
  const choice = consentChoice.getStore();
  return choice !== undefined && choice.userId === user.id && choice.sessionId === session.id ? choice.organizationId : undefined;
};

type Adapter = Parameters<Parameters<typeof createAuthMiddleware>[0]>[0]["context"]["adapter"];
type Grant = { readonly clientId: string; readonly userId: string; readonly referenceId: string };
type Consent = { readonly id: string; readonly createdAt: Date };

/** The consent standing for this client, user and Organization, which is the grant any new token mints under. */
const currentConsent = (adapter: Adapter, grant: Grant) =>
  adapter.findOne<Consent>({
    model: "oauthConsent",
    where: [
      { field: "clientId", value: grant.clientId },
      { field: "userId", value: grant.userId },
      { field: "referenceId", value: grant.referenceId },
    ],
  });

const revoked = () => new APIError("BAD_REQUEST", { error: "invalid_grant", error_description: "the connection was revoked" });

/**
 * The grant a token request may mint under, bound by the `/oauth2/token` before hook. better-auth merges a hook's
 * returned `context` onto the endpoint context, and the provider hands that same context to claim extensions, so the
 * binding travels with the request and nothing else.
 */
const BOUND_GRANT = "ployzBoundGrant";
const BoundGrant = Schema.Struct({ [BOUND_GRANT]: Schema.String });
const boundGrant = (ctx: OAuthClaimExtensionInput["ctx"]) =>
  Option.map(Schema.decodeUnknownOption(BoundGrant)(ctx), (bound) => bound[BOUND_GRANT]);

// The consent row's id names the grant, so revoking it and connecting again yields a new id the old token can't carry.
// A request mints only under the grant the hook bound to its credential. A request the hook bound nothing for mints
// nothing: for the code and refresh grants that means no credential row stood, which the provider refuses first anyway,
// and this Cloud issues no other token with a user and Organization behind it.
const grantClaims = async ({ ctx, user, client, referenceId }: OAuthClaimExtensionInput) => {
  if (user === undefined || user === null || referenceId === undefined) return {};
  const bound = boundGrant(ctx);
  if (Option.isNone(bound)) throw revoked();
  const consent = await currentConsent(ctx.context.adapter, { clientId: client.clientId, userId: user.id, referenceId });
  if (consent === null || consent.id !== bound.value) throw revoked();
  return { org: referenceId, grant: consent.id };
};

// better-auth stores codes and refresh tokens as their SHA-256, base64url without padding.
const stored = (token: string) => createHash("sha256").update(token).digest("base64url");
const seconds = (at: Date | string) => Math.floor(new Date(at).getTime() / 1000);

type Credential = Grant & { readonly createdAt: Date };
const TokenRequest = Schema.Union([
  Schema.Struct({ grant_type: Schema.Literal("authorization_code"), code: Schema.String }),
  Schema.Struct({ grant_type: Schema.Literal("refresh_token"), refresh_token: Schema.String }),
]);
const PendingCode = Schema.Struct({
  createdAt: Schema.Date,
  value: Schema.fromJsonString(Schema.Struct({
    userId: Schema.String,
    query: Schema.Struct({ client_id: Schema.String }),
    referenceId: Schema.String,
  })),
});
const Addressed = Schema.Struct({ resource: Schema.Union([Schema.String, Schema.Array(Schema.String)]) });
const RefreshToken = Schema.Struct({ createdAt: Schema.Date, clientId: Schema.String, userId: Schema.String, referenceId: Schema.String });

/** The credential a token request presents, as the row better-auth will read it from, or nothing when there's no such row to bind. */
const presented = async (adapter: Adapter, request: typeof TokenRequest.Type): Promise<Credential | undefined> => {
  if (request.grant_type === "authorization_code") {
    const row = await adapter.findOne({ model: "verification", where: [{ field: "identifier", value: stored(request.code) }] });
    const code = Schema.decodeUnknownOption(PendingCode)(row);
    if (Option.isNone(code)) return undefined;
    const { createdAt, value } = code.value;
    return { createdAt, clientId: value.query.client_id, userId: value.userId, referenceId: value.referenceId };
  }
  const row = await adapter.findOne({ model: "oauthRefreshToken", where: [{ field: "token", value: stored(request.refresh_token) }] });
  return Option.getOrUndefined(Schema.decodeUnknownOption(RefreshToken)(row));
};

// Consent ids are UUIDs. The endpoint looks any other id up in a uuid column and fails with a 500, so it's a 404 here.
const isConsentId = Schema.is(Schema.String.check(Schema.isUUID()));
const DeleteConsent = Schema.Struct({ id: Schema.String });

/**
 * Every credential belongs to the grant that stood when it was made. Disconnecting deletes that consent, and connecting
 * again makes a newer one, so a code or refresh token older than the current consent is from a revoked grant and mints
 * nothing. Timestamps are whole seconds, so only a strictly older credential is refused. The consent that passes is
 * bound to the request, so a disconnect and reconnect while the request is in flight can't hand it the newer grant.
 *
 * The binding is enforced where the provider resolves access token claims, which it does only for a JWT, and it mints
 * a JWT only for a request that names a resource. So a request must name the MCP resource, as the MCP spec has every
 * client do, or a resourceless one would mint an opaque token and a refresh row that skipped the binding.
 */
const bindCredentialsToGrant = (resource: string): BetterAuthPlugin => ({
  id: "ployz-bind-credentials-to-grant",
  hooks: {
    before: [
      {
        matcher: (ctx) => ctx.path === "/oauth2/token",
        handler: createAuthMiddleware(async (ctx) => {
          const request = Schema.decodeUnknownOption(TokenRequest)(ctx.body);
          if (Option.isNone(request)) return;
          const addressed = Schema.decodeUnknownOption(Addressed)(ctx.body);
          if (!Option.exists(addressed, (named) => [named.resource].flat().includes(resource))) {
            throw new APIError("BAD_REQUEST", { error: "invalid_target", error_description: `a token is only issued for ${resource}` });
          }
          const credential = await presented(ctx.context.adapter, request.value);
          if (credential === undefined) return;
          const consent = await currentConsent(ctx.context.adapter, credential);
          if (consent === null || seconds(credential.createdAt) < seconds(consent.createdAt)) throw revoked();
          return { context: { [BOUND_GRANT]: consent.id } };
        }),
      },
      {
        matcher: (ctx) => ctx.path === "/oauth2/delete-consent",
        handler: createAuthMiddleware(async (ctx) => {
          const asked = Schema.decodeUnknownOption(DeleteConsent)(ctx.body);
          if (Option.isSome(asked) && !isConsentId(asked.value.id)) {
            throw new APIError("NOT_FOUND", { error: "not_found", error_description: "no consent" });
          }
        }),
      },
    ],
  },
});

export const hostedMcpPlugins = (appUrl: URL) => {
  const { issuer, resource } = mcpEndpoints(appUrl);
  return [
    jwt({ jwt: { issuer }, disableSettingJwtHeader: true }),
    mcp({
      resource,
      scopes: [...MCP_SCOPES],
      loginPage: CONNECT_PAGE,
      consentPage: CONNECT_PAGE,
      allowDynamicClientRegistration: true,
      allowUnauthenticatedClientRegistration: true,
      accessTokenExpiresIn: 60 * 60,
      postLogin: {
        page: CONNECT_PAGE,
        shouldRedirect: (context) => chosenOrganization(context) === undefined,
        consentReferenceId: chosenOrganization,
      },
      extensions: [{ claims: { accessToken: grantClaims } }],
    }),
    cimd({ fetchClientMetadataResource, metadataProfile: "mcp-2026-07-28" }),
    bindCredentialsToGrant(resource),
  ];
};
