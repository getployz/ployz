import "@tanstack/react-start/server-only";
import crypto from "node:crypto";
import { Effect, Redacted, Schema } from "effect";
import {
  linkGithubInstallation,
  listCachedGithubRepositoriesForOrganization,
  listGithubInstallationsForOrganization,
  upsertGithubInstallation,
} from "#/modules/github/github.repository";
import type { ConnectNotice } from "#/modules/github/github-connect";
import { GithubApi } from "#/modules/github/github-observation.api";
import { getGithubAppInstallUrl } from "#/modules/github/github.server";
import { githubIdSchema } from "#/modules/github/github-ingestion.contracts";
import type { Actor } from "#/modules/identity/actor";
import { requireAccess, requireAccessById } from "#/modules/organization/access.server";
import { createGithubRepositoriesSyncRequestedEvent } from "#/modules/inngest/events";
import { sendInngestEvent } from "#/modules/inngest/client";
import { AppConfig } from "#/server/config.server";
import { Forbidden, Validation } from "#/server/public-error";

const TOKEN_TTL_MS = 15 * 60_000;

const ConnectState = Schema.Struct({ organizationId: Schema.String, userId: Schema.String, expiresAt: Schema.Number });
const ConnectOffer = Schema.Struct({
  ...ConnectState.fields,
  githubLogin: Schema.String,
  accounts: Schema.Array(Schema.Struct({ installationId: githubIdSchema, login: Schema.String, avatarUrl: Schema.NullOr(Schema.String) })),
});

type Signed<S extends Schema.Top> = { readonly purpose: string; readonly schema: S };
const STATE: Signed<typeof ConnectState> = { purpose: "ployz:github-connect-state:v1", schema: ConnectState };
const OFFER: Signed<typeof ConnectOffer> = { purpose: "ployz:github-connect-offer:v1", schema: ConnectOffer };

const keyFor = (secret: string, purpose: string) =>
  crypto.createHash("sha256").update(purpose).update("\0").update(secret).digest();

const sign = Effect.fn("GithubConnect.sign")(function* <S extends Schema.Top>(kind: Signed<S>, value: S["Type"]) {
  const secret = Redacted.value((yield* AppConfig).encryptionSecret);
  const body = Buffer.from(JSON.stringify(value)).toString("base64url");
  return `${body}.${crypto.createHmac("sha256", keyFor(secret, kind.purpose)).update(body).digest("base64url")}`;
});

const verify = Effect.fn("GithubConnect.verify")(function* <S extends Schema.Top & { readonly Type: { readonly expiresAt: number } }>(
  kind: Signed<S>, token: string,
) {
  const secret = Redacted.value((yield* AppConfig).encryptionSecret);
  const [body, given, ...rest] = token.split(".");
  if (body === undefined || given === undefined || rest.length > 0) return null;
  const expected = Buffer.from(crypto.createHmac("sha256", keyFor(secret, kind.purpose)).update(body).digest("base64url"));
  const actual = Buffer.from(given);
  if (actual.length !== expected.length || !crypto.timingSafeEqual(actual, expected)) return null;
  const value = yield* Schema.decodeUnknownEffect(Schema.fromJsonString(kind.schema))(Buffer.from(body, "base64url").toString()).pipe(
    Effect.orElseSucceed(() => null),
  );
  return value !== null && value.expiresAt > Date.now() ? value : null;
});

const settingsPath = (organizationSlug: string, search: { connect: ConnectNotice } | { offer: string }) =>
  `/cloud/${organizationSlug}/~/settings?${new URLSearchParams({ section: "github", ...search }).toString()}`;

export const startGithubConnect = Effect.fn("GithubConnect.start")(function* (actor: Actor, organizationSlug: string, existing: boolean) {
  const config = yield* AppConfig;
  const { organization } = yield* requireAccess(actor, organizationSlug, "manage");
  const state = yield* sign(STATE, { organizationId: organization.id, userId: actor.userId, expiresAt: Date.now() + TOKEN_TTL_MS });
  if (!existing) return `${getGithubAppInstallUrl(config.github)}?${new URLSearchParams({ state }).toString()}`;
  return `${config.github.origins.web}/login/oauth/authorize?${new URLSearchParams({
    client_id: config.github.appClientId,
    redirect_uri: new URL("/api/github/callback", config.app.url).href,
    state,
  }).toString()}`;
});

const linkChosen = Effect.fn("GithubConnect.linkChosen")(function* (input: {
  organizationId: string; userId: string; githubLogin: string; installationIds: ReadonlyArray<number>;
}) {
  for (const installationId of input.installationIds) {
    yield* linkGithubInstallation({
      organizationId: input.organizationId, installationId, connectedByUserId: input.userId, connectedGithubLogin: input.githubLogin,
    });
  }
  yield* sendInngestEvent(input.installationIds.map((installationId) =>
    createGithubRepositoriesSyncRequestedEvent({ installationId, reason: "manual-refresh" }))).pipe(
    Effect.catch((error) => Effect.logWarning("Linked a GitHub installation; its repositories sync on the next refresh.", error)),
  );
});

export const completeGithubConnect = Effect.fn("GithubConnect.complete")(function* (actor: Actor, params: URLSearchParams) {
  const state = yield* verify(STATE, params.get("state") ?? "");
  if (state === null || state.userId !== actor.userId) {
    return yield* new Forbidden({ message: "This GitHub link expired or isn't yours." });
  }
  const { organization } = yield* requireAccessById(actor, state.organizationId, "manage");
  const land = (search: Parameters<typeof settingsPath>[1]) => settingsPath(organization.slug, search);
  const code = params.get("code");
  if (code === null) return land({ connect: params.get("setup_action") === "request" ? "pending" : "failed" });

  const api = yield* GithubApi;
  const authorized = yield* api.userInstallations(code).pipe(
    Effect.tapError((error) => Effect.logWarning("GitHub did not complete a connect.", error)),
    Effect.orElseSucceed(() => null),
  );
  if (authorized === null) return land({ connect: "failed" });
  const reachable = authorized.installations.filter((installation) => installation.suspended_at === null);
  const requestedInstallationId = params.get("installation_id");
  const chosen = requestedInstallationId === null ? reachable : reachable.filter((installation) => String(installation.id) === requestedInstallationId);
  if (requestedInstallationId !== null && chosen.length === 0) return land({ connect: "not-yours" });

  const linked = new Set((yield* listGithubInstallationsForOrganization(organization.id)).map((row) => row.installationId));
  if (requestedInstallationId !== null && linked.has(Number(requestedInstallationId))) return land({ connect: "linked" });
  const candidates = chosen.filter((installation) => !linked.has(installation.id));
  for (const installation of candidates) {
    yield* upsertGithubInstallation({
      installationId: installation.id,
      accountLogin: installation.account.login,
      accountType: installation.account.type,
      accountAvatarUrl: installation.account.avatar_url,
    });
  }
  const [only, ...more] = candidates;
  if (only === undefined) return land({ connect: "none" });
  const base = { organizationId: organization.id, userId: actor.userId, githubLogin: authorized.login };
  if (more.length === 0) {
    yield* linkChosen({ ...base, installationIds: [only.id] });
    return land({ connect: "linked" });
  }
  const accounts = candidates.map((installation) => ({
    installationId: installation.id, login: installation.account.login, avatarUrl: installation.account.avatar_url,
  }));
  const offer = yield* sign(OFFER, { ...base, accounts, expiresAt: Date.now() + TOKEN_TTL_MS });
  return land({ offer });
});

const readOffer = Effect.fn("GithubConnect.readOffer")(function* (actor: Actor, organizationSlug: string, token: string) {
  const { organization } = yield* requireAccess(actor, organizationSlug, "manage");
  const offer = yield* verify(OFFER, token);
  if (offer === null || offer.userId !== actor.userId || offer.organizationId !== organization.id) {
    return yield* new Forbidden({ message: "This GitHub link expired or isn't yours." });
  }
  return offer;
});

export const listGithubOffer = Effect.fn("GithubConnect.listOffer")(function* (actor: Actor, organizationSlug: string, token: string) {
  return (yield* readOffer(actor, organizationSlug, token)).accounts;
});

export const acceptGithubOffer = Effect.fn("GithubConnect.acceptOffer")(function* (
  actor: Actor, input: { organizationSlug: string; offer: string; installationIds: ReadonlyArray<number> },
) {
  const offer = yield* readOffer(actor, input.organizationSlug, input.offer);
  const picked = [...new Set(input.installationIds)];
  if (picked.length === 0 || picked.some((id) => !offer.accounts.some((account) => account.installationId === id))) {
    return yield* new Validation({ message: "Pick from the accounts GitHub offered.", userFacing: true });
  }
  yield* linkChosen({ organizationId: offer.organizationId, userId: offer.userId, githubLogin: offer.githubLogin, installationIds: picked });
  return { linked: picked.length };
});

export const listOrganizationGithub = Effect.fn("GithubConnect.list")(function* (actor: Actor, organizationSlug: string) {
  const access = yield* requireAccess(actor, organizationSlug, "operate");
  const installations = yield* listGithubInstallationsForOrganization(access.organization.id);
  const repositories = yield* listCachedGithubRepositoriesForOrganization(access.organization.id);
  return {
    canManage: access.kind === "support" || access.role !== "member",
    accounts: installations.map((installation) => ({
      login: installation.accountLogin,
      avatarUrl: installation.accountAvatarUrl,
      repositories: repositories.filter((repository) => repository.installationId === installation.installationId).length,
      connectedBy: installation.connectedByName,
      connectedAt: installation.connectedAt.toISOString(),
    })),
  };
});
