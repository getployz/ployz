import { AsyncLocalStorage } from "node:async_hooks";
import { cimd } from "@better-auth/cimd";
import { fetchClientMetadataResource } from "@better-auth/cimd/node";
import { mcp } from "@better-auth/mcp";
import type { OAuthClaimExtensionInput } from "@better-auth/oauth-provider";
import { jwt } from "better-auth/plugins";

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
  ];
};
