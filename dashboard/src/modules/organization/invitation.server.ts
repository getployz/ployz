import "@tanstack/react-start/server-only";
import { randomBytes } from "node:crypto";
import { and, asc, eq, gt, inArray, ne, sql } from "drizzle-orm";
import { Effect } from "effect";
import { sha256Hex } from "#/lib/hash.server";
import { requireClaimed } from "#/modules/sandbox/claim-required.server";
import { user } from "#/modules/identity/tables";
import { invitation, member, organization } from "#/modules/organization/tables";
import { type InviteRole, MANAGING_ROLES } from "#/modules/organization/permissions";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";
import { DAY_MS } from "#/lib/time-range";

export const INVITE_DAYS = 7;

const hashInviteToken = (token: string) => sha256Hex(token);

type InviteState = "pending" | "accepted" | "revoked" | "expired";

const fromManager = sql<boolean>`exists (select 1 from ${member} where ${member.organizationId} = ${invitation.organizationId}
  and ${member.userId} = ${invitation.inviterId} and ${inArray(member.role, [...MANAGING_ROLES])})`;

/** Whether an invite has lapsed, on Postgres's clock, which also set its expiry. */
const expired = sql<boolean>`${invitation.expiresAt} <= now()`;

const stateOf = (row: { status: typeof invitation.$inferSelect.status; expired: boolean; fromManager: boolean }): InviteState => {
  if (row.status !== "pending") return row.status;
  if (!row.fromManager) return "revoked";
  return row.expired ? "expired" : "pending";
};

/** Makes an invite link. The token is returned here and never again; only its hash is kept. */
export const createInvite = Effect.fn("Invitation.create")(function* (
  organizationId: string,
  inviterId: string,
  input: { readonly email: string | null; readonly role: InviteRole },
) {
  yield* requireClaimed(organizationId, "invite");
  const { drizzle } = yield* Database;
  const email = input.email?.trim().toLowerCase() || null;
  if (email !== null) {
    const [joined] = yield* drizzle.select({ id: member.id }).from(member)
      .innerJoin(user, eq(user.id, member.userId))
      .where(and(eq(member.organizationId, organizationId), sql`lower(${user.email}) = ${email}`)).limit(1);
    if (joined !== undefined) return yield* new Conflict({ message: `${email} is already a member.`, userFacing: true });
    const [waiting] = yield* drizzle.select({ id: invitation.id }).from(invitation)
      .where(and(eq(invitation.organizationId, organizationId), eq(invitation.email, email), eq(invitation.status, "pending"), gt(invitation.expiresAt, sql`now()`), fromManager))
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
    expiresAt: sql`now() + make_interval(secs => ${INVITE_DAYS * DAY_MS / 1000})`,
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
    .where(and(eq(invitation.organizationId, organizationId), eq(invitation.status, "pending"), gt(invitation.expiresAt, sql`now()`), fromManager))
    .orderBy(asc(invitation.createdAt));
});

/**
 * Revokes a pending invite, selected by id or by the email it was labelled with. Revoking one already revoked is a
 * no-op; one already used refuses, since revoking it would not remove the member.
 */
export const revokeInvite = Effect.fn("Invitation.revoke")(function* (organizationId: string, selector: string) {
  const { drizzle } = yield* Database;
  const wanted = selector.trim().toLowerCase();
  const rows = yield* drizzle.select({ id: invitation.id, email: invitation.email, status: invitation.status, expired, fromManager })
    .from(invitation).where(eq(invitation.organizationId, organizationId));
  const byId = rows.find((row) => row.id === selector.trim());
  const found = byId ?? rows.find((row) => row.email === wanted && stateOf(row) === "pending");
  if (found === undefined) return yield* new NotFound({ message: `No pending invite matches ${selector}.` });
  const used = new Conflict({ message: "That invite was used already. Remove the member instead.", userFacing: true });
  if (found.status === "accepted") return yield* used;
  const revoked = yield* drizzle.update(invitation).set({ status: "revoked" })
    .where(and(eq(invitation.id, found.id), ne(invitation.status, "accepted"))).returning({ id: invitation.id });
  if (revoked.length === 0) return yield* used;
  return { id: found.id, email: found.email };
});

const byToken = (token: string) => eq(invitation.tokenHash, hashInviteToken(token));

/** Where the link leads, whether `userId` is in that Organization already, and whether they joined through it and left. */
export const readInvite = Effect.fn("Invitation.read")(function* (token: string, userId: string | null) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({
    organization: organization.name,
    slug: organization.slug,
    invitedBy: user.name,
    role: invitation.role,
    status: invitation.status,
    expired,
    acceptedBy: invitation.acceptedByUserId,
    fromManager,
    member: userId === null ? sql<boolean>`false` : sql<boolean>`exists (select 1 from ${member} where ${member.organizationId} = ${invitation.organizationId}
      and ${member.userId} = ${userId})`,
  }).from(invitation)
    .innerJoin(organization, eq(organization.id, invitation.organizationId))
    .innerJoin(user, eq(user.id, invitation.inviterId))
    .where(byToken(token)).limit(1);
  if (row === undefined) return null;
  const state = stateOf(row);
  // Whoever joined through this link and has since left or been removed.
  const membershipEnded = state === "accepted" && userId !== null && row.acceptedBy === userId && !row.member;
  return { organization: row.organization, slug: row.slug, invitedBy: row.invitedBy, role: row.role, state, member: row.member, membershipEnded };
});

const UNUSABLE = {
  accepted: "This invite was used already. Ask for a new one.",
  revoked: "This invite was revoked. Ask for a new one.",
  expired: "This invite expired. Ask for a new one.",
} satisfies Record<Exclude<InviteState, "pending">, string>;

/**
 * Makes `userId` a member through the invite and spends it. Someone already a member keeps their role and the invite
 * stays unspent. Locks the invite row, so two people opening one link cannot both join.
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
      expired,
    }).from(invitation)
      .innerJoin(organization, eq(organization.id, invitation.organizationId))
      .where(byToken(token)).for("update", { of: invitation }).limit(1);
    if (row === undefined) return yield* new NotFound({ message: "That invite does not exist." });
    const [already] = yield* drizzle.select({ id: member.id }).from(member)
      .where(and(eq(member.organizationId, row.organizationId), eq(member.userId, userId))).limit(1);
    if (already !== undefined) return { slug: row.slug };
    const [manager] = yield* drizzle.select({ id: member.id }).from(member)
      .where(and(eq(member.organizationId, row.organizationId), eq(member.userId, row.inviterId), inArray(member.role, [...MANAGING_ROLES]))).for("share");
    const state = stateOf({ ...row, fromManager: manager !== undefined });
    if (state !== "pending") return yield* new Conflict({ message: UNUSABLE[state], userFacing: true });
    // now() is when this transaction began, before it waited on the locks above: the expiry is checked again on the
    // clock as it reads once they are held.
    const [spent] = yield* drizzle.update(invitation).set({ status: "accepted", acceptedByUserId: userId })
      .where(and(eq(invitation.id, row.id), sql`${invitation.expiresAt} > clock_timestamp()`)).returning({ id: invitation.id });
    if (spent === undefined) return yield* new Conflict({ message: UNUSABLE.expired, userFacing: true });
    yield* drizzle.insert(member).values({ organizationId: row.organizationId, userId, role: row.role });
    return { slug: row.slug };
  }));
});
