import { AsyncLocalStorage } from "node:async_hooks";
import { cimd } from "@better-auth/cimd";
import { fetchClientMetadataResource } from "@better-auth/cimd/node";
import { mcp } from "@better-auth/mcp";
import type { OAuthClaimExtensionInput } from "@better-auth/oauth-provider";
import type { BetterAuthPlugin } from "better-auth";
import { APIError, createAuthMiddleware, getSessionFromCtx, isAPIError } from "better-auth/api";
import { jwt } from "better-auth/plugins";
import { Option, Schema } from "effect";

export const MCP_SCOPES = ["ployz", "offline_access"] as const;
export const CONNECT_PAGE = "/connect";

/** Where a hosted Cloud's MCP resource and its authorization server live. */
export const mcpEndpoints = (appUrl: URL) => ({
  issuer: new URL("/api/auth", appUrl).href,
  resource: new URL("/mcp", appUrl).href,
  jwks: new URL("/api/auth/jwks", appUrl).href,
});

export type ConsentChoice = { readonly userId: string; readonly sessionId: string; readonly organizationId: string };

const consentChoice = new AsyncLocalStorage<ConsentChoice>();

/** Runs a consent with the Organization the member picked on /connect, so the grant is bound to it. */
export const withConsentChoice = <A>(choice: ConsentChoice, run: () => Promise<A>) => consentChoice.run(choice, run);

const chosenOrganization = ({ user, session }: { user: { id: string }; session: { id: string } }) => {
  const choice = consentChoice.getStore();
  return choice !== undefined && choice.userId === user.id && choice.sessionId === session.id ? choice.organizationId : undefined;
};

// The consent row's id names the grant, so revoking it and connecting again yields a new id the old token can't carry.
// No consent means the grant was revoked, so a code or refresh token from it mints nothing, not even a refresh token.
const grantClaims = async ({ ctx, user, client, referenceId }: OAuthClaimExtensionInput) => {
  if (user === undefined || user === null || referenceId === undefined) return {};
  const consent = await ctx.context.adapter.findOne<{ id: string }>({
    model: "oauthConsent",
    where: [
      { field: "clientId", value: client.clientId },
      { field: "userId", value: user.id },
      { field: "referenceId", value: referenceId },
    ],
  });
  if (consent === null) throw new APIError("BAD_REQUEST", { error: "invalid_grant", error_description: "the connection was revoked" });
  return { org: referenceId, grant: consent.id };
};

// Consent ids are UUIDs. The endpoint looks any other id up in a uuid column and fails with a 500, so it's a 404 here.
const isConsentId = Schema.is(Schema.String.check(Schema.isUUID()));
const DeleteConsent = Schema.Struct({ id: Schema.String });

type Grant = { readonly clientId: string; readonly userId: string; readonly referenceId?: string | null };

const PendingCode = Schema.fromJsonString(Schema.Struct({
  query: Schema.Struct({ client_id: Schema.String }),
  referenceId: Schema.optional(Schema.NullOr(Schema.String)),
}));

type Adapter = Parameters<Parameters<typeof createAuthMiddleware>[0]>[0]["context"]["adapter"];

const revokeGrant = async (adapter: Adapter, grant: Grant) => {
  const referenceId = grant.referenceId ?? undefined;
  const family = [
    { field: "clientId", value: grant.clientId },
    { field: "userId", value: grant.userId },
    ...(referenceId === undefined ? [] : [{ field: "referenceId", value: referenceId }]),
  ];
  const refreshTokens = await adapter.findMany<{ id: string }>({ model: "oauthRefreshToken", where: family });
  if (refreshTokens.length > 0) {
    await adapter.deleteMany({ model: "oauthAccessToken", where: [{ field: "refreshId", operator: "in", value: refreshTokens.map((token) => token.id) }] });
  }
  await adapter.deleteMany({ model: "oauthRefreshToken", where: family });
  const codes = await adapter.findMany<{ id: string; value: string }>({
    model: "verification",
    where: [
      { field: "value", operator: "contains", value: '"type":"authorization_code"' },
      { field: "value", operator: "contains", value: `"userId":${JSON.stringify(grant.userId)}` },
    ],
  });
  const granted = codes.filter((code) =>
    Schema.decodeUnknownOption(PendingCode)(code.value).pipe(
      Option.exists(({ query, referenceId: codeReference }) => query.client_id === grant.clientId && (codeReference ?? undefined) === referenceId),
    ),
  );
  if (granted.length > 0) {
    await adapter.deleteMany({ model: "verification", where: [{ field: "id", operator: "in", value: granted.map((code) => code.id) }] });
  }
};

// Grants being disconnected, keyed by consent id, carried from the hook before the consent is deleted to the one after.
const disconnecting = new Map<string, Grant>();

// better-auth deletes only the consent row on disconnect, so the grant's refresh tokens and unredeemed codes would mint
// again after a reconnect. They're purged once the consent is gone, so anything minted while it was going is caught too.
const revokeOnDisconnect = {
  id: "ployz-revoke-on-disconnect",
  hooks: {
    before: [
      {
        matcher: (ctx) => ctx.path === "/oauth2/delete-consent",
        handler: createAuthMiddleware(async (ctx) => {
          const session = await getSessionFromCtx(ctx);
          if (session === null) return;
          const asked = Schema.decodeUnknownOption(DeleteConsent)(ctx.body);
          if (Option.isNone(asked)) return;
          const { id } = asked.value;
          if (!isConsentId(id)) throw new APIError("NOT_FOUND", { error: "not_found", error_description: "no consent" });
          const consent = await ctx.context.adapter.findOne<Grant>({ model: "oauthConsent", where: [{ field: "id", value: id }] });
          if (consent === null || consent.userId !== session.user.id) return;
          disconnecting.set(id, consent);
        }),
      },
    ],
    after: [
      {
        matcher: (ctx) => ctx.path === "/oauth2/delete-consent",
        handler: createAuthMiddleware(async (ctx) => {
          const asked = Schema.decodeUnknownOption(DeleteConsent)(ctx.body);
          if (Option.isNone(asked)) return;
          const grant = disconnecting.get(asked.value.id);
          disconnecting.delete(asked.value.id);
          if (grant === undefined || isAPIError(ctx.context.returned)) return;
          await revokeGrant(ctx.context.adapter, grant);
        }),
      },
    ],
  },
} satisfies BetterAuthPlugin;

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
    revokeOnDisconnect,
  ];
};
