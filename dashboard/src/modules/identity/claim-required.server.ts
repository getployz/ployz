import "@tanstack/react-start/server-only";
import crypto from "node:crypto";
import { and, eq } from "drizzle-orm";
import { Effect, Redacted } from "effect";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { member, user } from "#/modules/identity/tables";
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

export const claimUrlFor = Effect.fn("Identity.claimUrlFor")(function* (organizationId: string) {
  const config = yield* AppConfig;
  const key = crypto.createHash("sha256").update(CLAIM_KEY_PURPOSE).update("\0").update(Redacted.value(config.encryptionSecret)).digest();
  const secret = crypto.createHmac("sha256", key).update(organizationId).digest("base64url");
  return new URL(`/claim/${secret}`, config.app.url).href;
});

export const requireClaimed = Effect.fn("Identity.requireClaimed")(function* (organizationId: string, action: ClaimedAction) {
  if (!(yield* isAnonymousOrganization(organizationId))) return;
  const claimUrl = yield* claimUrlFor(organizationId);
  return yield* new StoreRefused({
    code: "claim_required",
    message: `Claim this Organization to ${wanted[action]}.`,
    details: { claim_url: claimUrl, action, next: `Open ${claimUrl} and sign in with GitHub, then try again.` },
  });
});
