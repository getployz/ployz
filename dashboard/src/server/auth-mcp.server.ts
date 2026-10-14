import { AsyncLocalStorage } from "node:async_hooks";
import { cimd } from "@better-auth/cimd";
import { fetchClientMetadataResource } from "@better-auth/cimd/node";
import { mcp } from "@better-auth/mcp";
import type { OAuthClaimExtensionInput } from "@better-auth/oauth-provider";
import type { BetterAuthPlugin } from "better-auth";
import { createAuthMiddleware, getSessionFromCtx } from "better-auth/api";
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
  return consent === null ? {} : { org: referenceId, grant: consent.id };
};

const DeleteConsent = Schema.Struct({ id: Schema.String });

type Grant = { readonly clientId: string; readonly userId: string; readonly referenceId?: string | null };

// better-auth deletes only the consent row on disconnect, so a refresh token from that grant would mint again after a reconnect.
const revokeOnDisconnect = {
  id: "ployz-revoke-on-disconnect",
  hooks: {
    before: [
      {
        matcher: (ctx) => ctx.path === "/oauth2/delete-consent",
        handler: createAuthMiddleware(async (ctx) => {
          const asked = Schema.decodeUnknownOption(DeleteConsent)(ctx.body);
          const session = await getSessionFromCtx(ctx);
          if (Option.isNone(asked) || session === null) return;
          const { id } = asked.value;
          const consent = await ctx.context.adapter.findOne<Grant>({ model: "oauthConsent", where: [{ field: "id", value: id }] });
          if (consent === null || consent.userId !== session.user.id) return;
          const family = [
            { field: "clientId", value: consent.clientId },
            { field: "userId", value: consent.userId },
            ...(consent.referenceId === undefined || consent.referenceId === null ? [] : [{ field: "referenceId", value: consent.referenceId }]),
          ];
          const refreshTokens = await ctx.context.adapter.findMany<{ id: string }>({ model: "oauthRefreshToken", where: family });
          if (refreshTokens.length > 0) {
            await ctx.context.adapter.deleteMany({
              model: "oauthAccessToken",
              where: [{ field: "refreshId", operator: "in", value: refreshTokens.map((token) => token.id) }],
            });
          }
          await ctx.context.adapter.deleteMany({ model: "oauthRefreshToken", where: family });
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
