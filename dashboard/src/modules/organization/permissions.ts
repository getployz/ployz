/** Support is Ployz staff acting in an Organization that allows support. */
export type PermissionRole = "owner" | "admin" | "member" | "support";

type RankedRole = Exclude<PermissionRole, "support">;

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

/** Everything `role` holds, in `PERMISSIONS` order: what the Store is told a caller may do. */
export const permissionsOf = (role: PermissionRole): Permission[] =>
  Object.hasOwn(ROLE_PERMISSIONS, role) ? PERMISSIONS.filter((permission) => ROLE_PERMISSIONS[role].has(permission)) : [];
