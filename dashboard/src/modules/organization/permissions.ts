/** What Ployz support may read inside an Organization beyond its shape. Each is off until a member turns it on. */
export type SupportShows = { readonly variables: boolean; readonly configs: boolean; readonly logs: boolean };

import { Schema } from "effect";

/** The roles a member holds, most trusted first. */
export const MemberRole = Schema.Literals(["owner", "admin", "member"]);
export type MemberRole = typeof MemberRole.Type;

/** The roles an invite may grant: an owner is made by an existing owner, never by a link. */
export const InviteRole = Schema.Literals(["admin", "member"]);
export type InviteRole = typeof InviteRole.Type;

/** Support is Ployz staff acting in an Organization that allows support. */
export type PermissionRole = MemberRole | "support";

type RankedRole = MemberRole;

const LEAST_ROLE = [
  ["org:read", "member"],
  ["org:settings", "admin"],
  ["org:billing", "owner"],
  ["org:delete", "owner"],
  ["members:read", "member"],
  ["members:manage", "admin"],
  ["tokens:create", "member"],
  ["tokens:manage", "admin"],
  ["agents:manage", "admin"],
  ["github:connect", "admin"],
  ["approvals:answer", "member"],
  ["audit:read", "admin"],
  ["project:read", "member"],
  ["project:create", "member"],
  ["project:edit", "member"],
  ["project:delete", "admin"],
  ["environment:create", "member"],
  ["environment:delete", "admin"],
  ["environments:protect", "admin"],
  ["changes:stage", "member"],
  ["changes:deploy", "member"],
  ["changes:destroy", "member"],
  ["changes:merge", "member"],
  ["services:control", "member"],
  ["variables:read", "member"],
  ["logs:read", "member"],
  ["metrics:read", "member"],
  ["volumes:read", "member"],
  ["volumes:delete-copy", "admin"],
  ["servers:read", "member"],
  ["servers:add", "admin"],
  ["servers:operate", "admin"],
  ["servers:remove", "admin"],
  ["servers:shell", "admin"],
] as const satisfies ReadonlyArray<readonly [string, RankedRole]>;

export type Permission = (typeof LEAST_ROLE)[number][0];

export const PERMISSIONS: ReadonlyArray<Permission> = LEAST_ROLE.map(([permission]) => permission);

/** Whether `permission` only reads. */
export const reads = (permission: Permission) => permission.endsWith(":read");

const RANK = { member: 0, admin: 1, owner: 2 } satisfies Record<RankedRole, number>;

/** Admin permissions support doesn't hold. Support keeps the manage set it had before these existed. */
const WITHHELD_FROM_SUPPORT: ReadonlyArray<Permission> = ["environments:protect"];

export const ROLE_PERMISSIONS = {
  owner: holdings("owner"),
  admin: holdings("admin"),
  member: holdings("member"),
  support: holdings("admin", WITHHELD_FROM_SUPPORT),
} as const satisfies Record<PermissionRole, ReadonlySet<Permission>>;

function holdings(role: RankedRole, withheld: ReadonlyArray<Permission> = []): ReadonlySet<Permission> {
  return new Set(LEAST_ROLE
    .filter(([permission, least]) => RANK[least] <= RANK[role] && !withheld.includes(permission))
    .map(([permission]) => permission));
}

// member.role is unchecked text, so a stored role outside the four holds nothing rather than throwing.
export const can = (role: PermissionRole, permission: Permission): boolean =>
  Object.hasOwn(ROLE_PERMISSIONS, role) && ROLE_PERMISSIONS[role].has(permission);

/** The roles a member manager holding `role` may give or take away: only an owner makes or removes an owner. */
export const assignableRoles = (role: MemberRole): ReadonlyArray<MemberRole> => (role === "owner" ? MemberRole.literals : InviteRole.literals);

/** The roles that may manage members: what an invite's maker must still hold for the invite to work. */
export const MANAGING_ROLES: ReadonlyArray<MemberRole> = MemberRole.literals.filter((role) => can(role, "members:manage"));

/** Everything `role` holds, in `PERMISSIONS` order: what the Store is told a caller may do. */
export const permissionsOf = (role: PermissionRole): Permission[] =>
  Object.hasOwn(ROLE_PERMISSIONS, role) ? PERMISSIONS.filter((permission) => ROLE_PERMISSIONS[role].has(permission)) : [];

/** What a gated control says it does when its refusal names the action instead of the Organization. */
type RefusalAction =
  | "add servers"
  | "enroll or reject servers"
  | "change how a server runs"
  | "upgrade servers"
  | "remove servers"
  | "forget servers"
  | "forget them"
  | "change these"
  | "change this"
  | "change where builds run"
  | "delete it";

/**
 * What a refusal says to someone whose role doesn't hold `permission`: the least role that does, and with `action`,
 * what they may not do. Server and screen share it, so a control and the call behind it refuse in the same words.
 */
export const refusalFor = (permission: Permission, action?: RefusalAction) => {
  const who = can("admin", permission) ? "Only an owner or admin" : "Only an owner";
  return action === undefined ? `${who} of this Organization can do this.` : `${who} can ${action}.`;
};
