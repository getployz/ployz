/** The roles that hold permissions. Support is Ployz staff in an Organization that allows it. */
export type PermissionRole = "owner" | "admin" | "member" | "support";

/** Each permission and the least role that holds it. Every role holds what the roles below it hold. */
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
] as const satisfies ReadonlyArray<readonly [string, "member" | "admin" | "owner"]>;

export type Permission = (typeof LEAST_ROLE)[number][0];

export const PERMISSIONS: ReadonlyArray<Permission> = LEAST_ROLE.map(([permission]) => permission);

/** Support keeps the set an admin holds, so it passes every gate today's "manage" passes and never "own". */
const RANK = { member: 0, admin: 1, support: 1, owner: 2 } satisfies Record<PermissionRole, number>;

export const ROLE_PERMISSIONS = {
  owner: holdings("owner"),
  admin: holdings("admin"),
  member: holdings("member"),
  support: holdings("support"),
} as const satisfies Record<PermissionRole, ReadonlySet<Permission>>;

function holdings(role: PermissionRole): ReadonlySet<Permission> {
  return new Set(LEAST_ROLE.filter(([, least]) => RANK[least] <= RANK[role]).map(([permission]) => permission));
}

export const can = (role: PermissionRole, permission: Permission): boolean => ROLE_PERMISSIONS[role].has(permission);
