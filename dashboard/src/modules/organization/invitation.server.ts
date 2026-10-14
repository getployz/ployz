import "@tanstack/react-start/server-only";
import { createHash, randomBytes } from "node:crypto";
import { and, asc, eq, gt, ne, sql } from "drizzle-orm";
import { Effect } from "effect";
import { invitation, member, user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";

const INVITE_DAYS = 7;
const DAY_MS = 24 * 60 * 60 * 1000;

type InviteRole = typeof invitation.$inferSelect.role;

const hashInviteToken = (token: string) => createHash("sha256").update(token).digest("hex");

type InviteState = "pending" | "accepted" | "revoked" | "expired";

const stateOf = (row: { status: typeof invitation.$inferSelect.status; expiresAt: Date }, now: Date): InviteState =>
  row.status === "pending" && row.expiresAt <= now ? "expired" : row.status;

/** Makes an invite link. The token is returned here and never again; only its hash is kept. */
export const createInvite = Effect.fn("Invitation.create")(function* (
  organizationId: string,
  inviterId: string,
  input: { readonly email: string | null; readonly role: InviteRole },
) {
  const { drizzle } = yield* Database;
  const email = input.email?.trim().toLowerCase() || null;
  if (email !== null) {
    const [joined] = yield* drizzle.select({ id: member.id }).from(member)
      .innerJoin(user, eq(user.id, member.userId))
      .where(and(eq(member.organizationId, organizationId), sql`lower(${user.email}) = ${email}`)).limit(1);
    if (joined !== undefined) return yield* new Conflict({ message: `${email} is already a member.`, userFacing: true });
    const [waiting] = yield* drizzle.select({ id: invitation.id }).from(invitation)
      .where(and(eq(invitation.organizationId, organizationId), eq(invitation.email, email), eq(invitation.status, "pending"), gt(invitation.expiresAt, new Date())))
      .limit(1);
    if (waiting !== undefined) return yield* new Conflict({ message: `${email} has a pending invite. Revoke it to send a new one.`, userFacing: true });
  }
  const token = randomBytes(32).toString("base64url");
  const [created] = yield* drizzle.insert(invitation).values({
    organizationId,
    inviterId,
    email,
    role: input.role,
    tokenHash: hashInviteToken(token),
    expiresAt: new Date(Date.now() + INVITE_DAYS * DAY_MS),
  }).returning({ id: invitation.id, expiresAt: invitation.expiresAt });
  if (created === undefined) return yield* Effect.die("insert returned no row");
  return { id: created.id, email, role: input.role, expiresAt: created.expiresAt, token };
});

export const listPendingInvites = Effect.fn("Invitation.listPending")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  return yield* drizzle.select({
    id: invitation.id,
    email: invitation.email,
    role: invitation.role,
    invitedBy: user.name,
    createdAt: invitation.createdAt,
    expiresAt: invitation.expiresAt,
  }).from(invitation)
    .innerJoin(user, eq(user.id, invitation.inviterId))
    .where(and(eq(invitation.organizationId, organizationId), eq(invitation.status, "pending"), gt(invitation.expiresAt, new Date())))
    .orderBy(asc(invitation.createdAt));
});

/**
 * Revokes a pending invite, selected by id or by the email it was labelled with. Revoking one already revoked is a
 * no-op; one already used refuses, since revoking it would not remove the member.
 */
export const revokeInvite = Effect.fn("Invitation.revoke")(function* (organizationId: string, selector: string) {
  const { drizzle } = yield* Database;
  const wanted = selector.trim().toLowerCase();
  const rows = yield* drizzle.select({ id: invitation.id, email: invitation.email, status: invitation.status, expiresAt: invitation.expiresAt })
    .from(invitation).where(eq(invitation.organizationId, organizationId));
  const now = new Date();
  const byId = rows.find((row) => row.id === selector.trim());
  const found = byId ?? rows.find((row) => row.email === wanted && stateOf(row, now) === "pending");
  if (found === undefined) return yield* new NotFound({ message: `No pending invite matches ${selector}.` });
  const used = new Conflict({ message: "That invite was used already. Remove the member instead.", userFacing: true });
  if (found.status === "accepted") return yield* used;
  const revoked = yield* drizzle.update(invitation).set({ status: "revoked" })
    .where(and(eq(invitation.id, found.id), ne(invitation.status, "accepted"))).returning({ id: invitation.id });
  if (revoked.length === 0) return yield* used;
  return { id: found.id, email: found.email };
});

const byToken = (token: string) => eq(invitation.tokenHash, hashInviteToken(token));

export const readInvite = Effect.fn("Invitation.read")(function* (token: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({
    organization: organization.name,
    slug: organization.slug,
    invitedBy: user.name,
    role: invitation.role,
    status: invitation.status,
    expiresAt: invitation.expiresAt,
  }).from(invitation)
    .innerJoin(organization, eq(organization.id, invitation.organizationId))
    .innerJoin(user, eq(user.id, invitation.inviterId))
    .where(byToken(token)).limit(1);
  if (row === undefined) return null;
  return { organization: row.organization, slug: row.slug, invitedBy: row.invitedBy, role: row.role, state: stateOf(row, new Date()) };
});

const UNUSABLE = {
  accepted: "This invite was used already. Ask for a new one.",
  revoked: "This invite was revoked. Ask for a new one.",
  expired: "This invite expired. Ask for a new one.",
} satisfies Record<Exclude<InviteState, "pending">, string>;

/**
 * Makes `userId` a member through the invite and spends it. Someone already a member keeps their role and the invite
 * stays unspent. Locks the invite row, so two people opening one link cannot both join, and share-locks the inviter's
 * membership: an invite from someone no longer an owner or admin there reads as revoked.
 */
export const acceptInvite = Effect.fn("Invitation.accept")(function* (token: string, userId: string) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.select({
      id: invitation.id,
      organizationId: invitation.organizationId,
      inviterId: invitation.inviterId,
      slug: organization.slug,
      role: invitation.role,
      status: invitation.status,
      expiresAt: invitation.expiresAt,
    }).from(invitation)
      .innerJoin(organization, eq(organization.id, invitation.organizationId))
      .where(byToken(token)).for("update", { of: invitation }).limit(1);
    if (row === undefined) return yield* new NotFound({ message: "That invite does not exist." });
    const [already] = yield* drizzle.select({ id: member.id }).from(member)
      .where(and(eq(member.organizationId, row.organizationId), eq(member.userId, userId))).limit(1);
    if (already !== undefined) return { slug: row.slug };
    const state = stateOf(row, new Date());
    if (state !== "pending") return yield* new Conflict({ message: UNUSABLE[state], userFacing: true });
    const [inviter] = yield* drizzle.select({ role: member.role }).from(member)
      .where(and(eq(member.organizationId, row.organizationId), eq(member.userId, row.inviterId))).for("share");
    if (inviter?.role !== "owner" && inviter?.role !== "admin") return yield* new Conflict({ message: UNUSABLE.revoked, userFacing: true });
    yield* drizzle.insert(member).values({ organizationId: row.organizationId, userId, role: row.role });
    yield* drizzle.update(invitation).set({ status: "accepted", acceptedByUserId: userId }).where(eq(invitation.id, row.id));
    return { slug: row.slug };
  }));
});
