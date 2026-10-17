import "@tanstack/react-start/server-only";
import crypto from "node:crypto";
import { and, eq, sql } from "drizzle-orm";
import { Effect, Redacted } from "effect";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { member, user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";

const CLAIM_KEY_PURPOSE = "ployz:claim:v1";

export type ClaimedAction = "add_domain" | "mint_token" | "invite" | "billing" | "github_connect";

const wanted = {
  add_domain: "add a custom domain",
  mint_token: "create a token",
  invite: "invite a member",
  billing: "manage billing",
  github_connect: "connect GitHub",
} satisfies Record<ClaimedAction, string>;

const ownerKinds = Effect.fn("Identity.ownerKinds")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .selectDistinct({ anonymous: user.isAnonymous })
    .from(member)
    .innerJoin(user, eq(user.id, member.userId))
    .where(and(eq(member.organizationId, organizationId), eq(member.role, "owner")));
  return rows.map((row) => row.anonymous);
});

export const isAnonymousOrganization = (organizationId: string) =>
  ownerKinds(organizationId).pipe(Effect.map((kinds) => kinds.length === 0 || kinds.includes(true)));

export const isOwnedOnlyByAgents = (organizationId: string) =>
  ownerKinds(organizationId).pipe(Effect.map((kinds) => kinds.includes(true) && !kinds.includes(false)));

/** The secret in the Organization's claim URL. Derived from its id, so every refusal names the same one. */
export const claimSecretFor = Effect.fn("Identity.claimSecretFor")(function* (organizationId: string) {
  const config = yield* AppConfig;
  const key = crypto.createHash("sha256").update(CLAIM_KEY_PURPOSE).update("\0").update(Redacted.value(config.encryptionSecret)).digest();
  return crypto.createHmac("sha256", key).update(organizationId).digest("base64url");
});

/** What `organization.claim_secret_hash` holds, so the claim page finds the Organization without the secret at rest. */
export const hashClaimSecret = (secret: string) => crypto.createHash("sha256").update(secret).digest("hex");

export const claimUrlForSecret = Effect.fn("Identity.claimUrlForSecret")(function* (secret: string) {
  const config = yield* AppConfig;
  return new URL(`/claim/${secret}`, config.app.url).href;
});

/**
 * The URL a person opens to claim the Organization. After the encryption secret rotates, the stored hash still names the
 * old secret; this moves it to the current one, so the URL handed out is the one that claims.
 */
export const claimUrlFor = Effect.fn("Identity.claimUrlFor")(function* (organizationId: string) {
  const secret = yield* claimSecretFor(organizationId);
  const hash = hashClaimSecret(secret);
  const { drizzle } = yield* Database;
  yield* drizzle.update(organization).set({ claimSecretHash: hash })
    .where(and(eq(organization.id, organizationId), sql`${organization.claimSecretHash} is distinct from ${hash}`));
  return yield* claimUrlForSecret(secret);
});

/** Refuses `claim_required` while the Organization is anonymous, naming the claim URL a person opens first. */
export const requireClaimed = Effect.fn("Identity.requireClaimed")(function* (organizationId: string, action: ClaimedAction) {
  if (!(yield* isAnonymousOrganization(organizationId))) return;
  const claimUrl = yield* claimUrlFor(organizationId);
  return yield* new StoreRefused({
    code: "claim_required",
    message: `Claim this Organization to ${wanted[action]}.`,
    details: { claim_url: claimUrl, action, next: `Open ${claimUrl} and sign in with GitHub, then try again.` },
  });
});
