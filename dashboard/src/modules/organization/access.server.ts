import "@tanstack/react-start/server-only";
import { and, eq, type SQL } from "drizzle-orm";
import { Context, Effect, Layer } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { member } from "#/modules/identity/tables";
import { organizationSupportAccess } from "#/modules/support-access/tables";
import { Auth } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { Forbidden, NotFound, Unauthorized } from "#/server/public-error";
import { organization } from "./tables";

type Role = typeof member.$inferSelect.role;
type OrganizationRef = { readonly id: string; readonly slug: string };

/** A member acting in their Organization, or Ployz support in one that allows it, under the staff member's own user id. */
type Access =
  | { readonly kind: "member"; readonly userId: string; readonly role: Role; readonly organization: OrganizationRef }
  | { readonly kind: "support"; readonly userId: string; readonly organization: OrganizationRef };

/** "operate": any member, or support. "manage": an owner or admin, or support. */
type Need = "operate" | "manage";

/** What the gate reads about one Organization for one user. `role` is null when they aren't a member. */
type AccessFacts = { readonly organization: OrganizationRef; readonly role: Role | null; readonly supportAllowed: boolean };

type AccessDecision = Access | { readonly kind: "forbidden" } | { readonly kind: "not_found" };

/** The one rule: members by role; staff only while the Organization allows support. Outsiders learn nothing. */
export const decideAccess = (input: {
  readonly userId: string;
  readonly facts: AccessFacts | null;
  readonly isStaff: boolean;
  readonly need: Need;
}): AccessDecision => {
  const { userId, facts, isStaff, need } = input;
  if (facts === null) return { kind: "not_found" };
  if (facts.role !== null) {
    if (need === "manage" && facts.role === "member") return { kind: "forbidden" };
    return { kind: "member", userId, role: facts.role, organization: facts.organization };
  }
  if (isStaff && facts.supportAllowed) return { kind: "support", userId, organization: facts.organization };
  return { kind: "not_found" };
};

type SupportStaffService = {
  /** Whether this Cloud has any support staff, so the setting means something. */
  readonly available: boolean;
  readonly isStaff: (userId: string) => boolean;
};

export const supportStaffOf = (userIds: ReadonlyArray<string>): SupportStaffService => {
  const staff = new Set(userIds);
  return { available: staff.size > 0, isStaff: (userId) => staff.has(userId) };
};

/** Ployz support staff, from `PLOYZ_SUPPORT_STAFF_USER_IDS`. Nobody wherever no layer provides it (tests). */
export const SupportStaff = Context.Reference<SupportStaffService>("ployz/SupportStaff", {
  defaultValue: () => supportStaffOf([]),
});

export const SupportStaffLive = Layer.effect(
  SupportStaff,
  Effect.map(AppConfig, (config) => supportStaffOf(config.supportStaffUserIds)),
);

const gate = Effect.fn("Access.gate")(function* (actor: Actor, which: SQL, need: Need) {
  const staff = yield* SupportStaff;
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle
    .select({ id: organization.id, slug: organization.slug, role: member.role, supportAllowed: organizationSupportAccess.allowed })
    .from(organization)
    .leftJoin(member, and(eq(member.organizationId, organization.id), eq(member.userId, actor.userId)))
    .leftJoin(organizationSupportAccess, eq(organizationSupportAccess.organizationId, organization.id))
    .where(which)
    .limit(1);
  const decision = decideAccess({
    userId: actor.userId,
    facts: row === undefined ? null : { organization: { id: row.id, slug: row.slug }, role: row.role, supportAllowed: row.supportAllowed ?? false },
    isStaff: staff.isStaff(actor.userId),
    need,
  });
  switch (decision.kind) {
    case "member":
    case "support":
      return decision;
    case "forbidden":
      return yield* new Forbidden({ message: "Only an owner or admin of this Organization can do this." });
    case "not_found":
      return yield* new NotFound({ message: "The organization was not found." });
  }
});

/** The actor's access to the Organization named `organizationSlug`: NotFound unless they may reach it at all. */
export const requireAccess = Effect.fn("Access.require")(function* (actor: Actor, organizationSlug: string, need: Need) {
  return yield* gate(actor, eq(organization.slug, organizationSlug), need);
});

export const requireAccessById = Effect.fn("Access.requireById")(function* (actor: Actor, organizationId: string, need: Need) {
  return yield* gate(actor, eq(organization.id, organizationId), need);
});

/** A route's signed-in user, gated on the Organization its URL names. */
export const authorizeRequest = Effect.fn("Access.authorizeRequest")(function* (headers: Headers, organizationSlug: string, need: Need) {
  const auth = yield* Auth;
  const session = yield* auth.getSession(headers);
  if (session === null) return yield* new Unauthorized();
  return yield* requireAccess({ userId: session.user.id }, organizationSlug, need);
});

const organizationColumns = {
  id: organization.id,
  name: organization.name,
  slug: organization.slug,
  logo: organization.logo,
};

/**
 * The Organizations the actor may open: their own, then, for staff, every one that allows support. A listing is never a
 * grant; opening one re-runs the gate.
 */
export const listAccessibleOrganizations = Effect.fn("Access.listAccessibleOrganizations")(function* (actor: Actor) {
  const { drizzle } = yield* Database;
  const staff = yield* SupportStaff;
  const memberships = yield* drizzle
    .select(organizationColumns)
    .from(member)
    .innerJoin(organization, eq(member.organizationId, organization.id))
    .where(eq(member.userId, actor.userId))
    .orderBy(organization.name);
  const own = memberships.map((row) => ({ ...row, access: "member" as const }));
  if (!staff.isStaff(actor.userId)) return own;
  const ownIds = new Set(own.map((row) => row.id));
  const allowing = yield* drizzle
    .select(organizationColumns)
    .from(organizationSupportAccess)
    .innerJoin(organization, eq(organizationSupportAccess.organizationId, organization.id))
    .where(eq(organizationSupportAccess.allowed, true))
    .orderBy(organization.name);
  return [
    ...own,
    ...allowing.filter((row) => !ownIds.has(row.id)).map((row) => ({ ...row, access: "support" as const })),
  ];
});
