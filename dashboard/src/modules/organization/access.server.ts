import "@tanstack/react-start/server-only";
import { and, eq, type SQL } from "drizzle-orm";
import { Context, Effect, Layer } from "effect";
import { type Actor, type Caller, callerOrSupportCan } from "#/modules/identity/actor";
import { FULL_GRANT, granted, type TokenGrant } from "#/modules/identity/token-grant";
import { member } from "#/modules/identity/tables";
import type { SupportShows } from "#/modules/support-access/support-access";
import { organizationSupportAccess } from "#/modules/support-access/tables";
import { Auth } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { Forbidden, NotFound, Unauthorized } from "#/server/public-error";
import { can, type Permission, permissionsOf } from "./permissions";
import { organization } from "./tables";

export type Role = typeof member.$inferSelect.role;
type OrganizationRef = { readonly id: string; readonly slug: string };

/** What an Access may do: its role's permissions narrowed by a token's grant, and the Environments it may write, if limited. */
type Held = { readonly permissions: Permission[]; readonly environments: string[] | undefined };

/**
 * A member acting in their Organization, or Ployz support in one that allows it, under the staff member's own user id.
 * Support reads only what `shows` lets it, as of this request.
 */
export type Access = Held & (
  | { readonly kind: "member"; readonly userId: string; readonly role: Role; readonly organization: OrganizationRef }
  | { readonly kind: "support"; readonly userId: string; readonly organization: OrganizationRef; readonly shows: SupportShows }
);

/** What the gate reads about one Organization for one user. `role` is null when they aren't a member. */
type AccessFacts = {
  readonly organization: OrganizationRef;
  readonly role: Role | null;
  readonly supportAllowed: boolean;
  readonly supportShows: SupportShows;
};

type Refused = { readonly kind: "forbidden"; readonly permission: Permission; readonly by: "role" | "grant" };
type AccessDecision = Access | Refused | { readonly kind: "not_found" };

/**
 * The one rule: members by role; staff only while the Organization allows support. Outsiders learn nothing. A token
 * holds only what its grant leaves of its maker's role.
 */
export const admit = (input: {
  readonly userId: string;
  readonly facts: AccessFacts | null;
  readonly isStaff: boolean;
  readonly permission: Permission;
  readonly grant?: TokenGrant;
}): AccessDecision => {
  const { userId, facts, isStaff, permission, grant = FULL_GRANT } = input;
  if (facts === null) return { kind: "not_found" };
  const hold = (role: Role | "support"): Held | Refused => {
    if (!can(role, permission)) return { kind: "forbidden", permission, by: "role" };
    const permissions = granted(permissionsOf(role), grant);
    if (!permissions.includes(permission)) return { kind: "forbidden", permission, by: "grant" };
    return { permissions, environments: grant.environments === null ? undefined : [...grant.environments] };
  };
  if (facts.role !== null) {
    const held = hold(facts.role);
    return "kind" in held ? held : { kind: "member", userId, role: facts.role, organization: facts.organization, ...held };
  }
  if (isStaff && facts.supportAllowed) {
    const held = hold("support");
    return "kind" in held ? held : { kind: "support", userId, organization: facts.organization, shows: facts.supportShows, ...held };
  }
  return { kind: "not_found" };
};

/** A refusal names the least role that holds `permission`, or, when the role holds it, the token's grant that doesn't. */
export const refusalOf = (permission: Permission, by: "role" | "grant" = "role") =>
  by === "grant"
    ? `This token's grant doesn't include ${permission}.`
    : can("admin", permission)
      ? "Only an owner or admin of this Organization can do this."
      : "Only an owner of this Organization can do this.";

/** Why `caller` can't use `permission`, worded for its role or its token's grant, or null when it can. */
export const lacks = (caller: Pick<Caller, "role" | "support" | "credential">, permission: Permission) => {
  if (callerOrSupportCan(caller, permission)) return null;
  return refusalOf(permission, callerOrSupportCan({ role: caller.role, support: caller.support }, permission) ? "grant" : "role");
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

const gate = Effect.fn("Access.gate")(function* (actor: Actor, which: SQL, permission: Permission) {
  const staff = yield* SupportStaff;
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle
    .select({
      id: organization.id,
      slug: organization.slug,
      role: member.role,
      supportAllowed: organizationSupportAccess.allowed,
      variables: organizationSupportAccess.variables,
      configs: organizationSupportAccess.configs,
      logs: organizationSupportAccess.logs,
    })
    .from(organization)
    .leftJoin(member, and(eq(member.organizationId, organization.id), eq(member.userId, actor.userId)))
    .leftJoin(organizationSupportAccess, eq(organizationSupportAccess.organizationId, organization.id))
    .where(which)
    .limit(1);
  const decision = admit({
    userId: actor.userId,
    facts:
      row === undefined
        ? null
        : {
            organization: { id: row.id, slug: row.slug },
            role: row.role,
            supportAllowed: row.supportAllowed ?? false,
            supportShows: { variables: row.variables ?? false, configs: row.configs ?? false, logs: row.logs ?? false },
          },
    isStaff: staff.isStaff(actor.userId),
    permission,
    grant: actor.credential?.kind === "token" ? actor.credential.grant : FULL_GRANT,
  });
  switch (decision.kind) {
    case "member":
    case "support":
      return decision;
    case "forbidden":
      return yield* new Forbidden({ message: refusalOf(decision.permission, decision.by), userFacing: true });
    case "not_found":
      return yield* new NotFound({ message: "The organization was not found." });
  }
});

/** The actor's access to the Organization named `organizationSlug`: NotFound unless they may reach it at all. */
export const requireAccess = Effect.fn("Access.require")(function* (actor: Actor, organizationSlug: string, permission: Permission) {
  return yield* gate(actor, eq(organization.slug, organizationSlug), permission);
});

export const requireAccessById = Effect.fn("Access.requireById")(function* (actor: Actor, organizationId: string, permission: Permission) {
  return yield* gate(actor, eq(organization.id, organizationId), permission);
});

/** A route's signed-in user, gated on the Organization its URL names. */
export const authorizeRequest = Effect.fn("Access.authorizeRequest")(function* (headers: Headers, organizationSlug: string, permission: Permission) {
  const auth = yield* Auth;
  const session = yield* auth.getSession(headers);
  if (session === null) return yield* new Unauthorized();
  return yield* requireAccess({ userId: session.user.id }, organizationSlug, permission);
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
