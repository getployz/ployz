import { describe, expect, it } from "vitest";
import { decideAccess, type Role } from "#/modules/organization/access.server";
import { can, PERMISSIONS, type Permission, type PermissionRole, ROLE_PERMISSIONS } from "#/modules/organization/permissions";
import { SUPPORT_SHOWS_NONE } from "#/modules/support-access/support-access";

// Appendix D of the permissions plan, row for row: member, admin, owner. Support holds what an admin holds.
const APPENDIX_D: ReadonlyArray<readonly [Permission, boolean, boolean, boolean]> = [
  ["org:read", true, true, true],
  ["org:settings", false, true, true],
  ["org:billing", false, false, true],
  ["org:delete", false, false, true],
  ["members:read", true, true, true],
  ["members:manage", false, true, true],
  ["tokens:create", true, true, true],
  ["tokens:manage", false, true, true],
  ["agents:manage", false, true, true],
  ["github:connect", false, true, true],
  ["approvals:answer", true, true, true],
  ["audit:read", false, true, true],
  ["project:read", true, true, true],
  ["project:create", true, true, true],
  ["project:edit", true, true, true],
  ["project:delete", false, true, true],
  ["environment:create", true, true, true],
  ["environment:delete", false, true, true],
  ["environments:protect", false, true, true],
  ["changes:stage", true, true, true],
  ["changes:deploy", true, true, true],
  ["changes:destroy", true, true, true],
  ["changes:merge", true, true, true],
  ["services:control", true, true, true],
  ["variables:read", true, true, true],
  ["logs:read", true, true, true],
  ["metrics:read", true, true, true],
  ["volumes:read", true, true, true],
  ["volumes:delete-copy", false, true, true],
  ["servers:read", true, true, true],
  ["servers:add", false, true, true],
  ["servers:operate", false, true, true],
  ["servers:remove", false, true, true],
  ["servers:shell", false, true, true],
];

const ROLES: ReadonlyArray<PermissionRole> = ["member", "admin", "owner", "support"];

describe("ROLE_PERMISSIONS", () => {
  it("names the 34 permissions of Appendix D in its order, for four roles", () => {
    expect(PERMISSIONS).toEqual(APPENDIX_D.map(([permission]) => permission));
    expect(PERMISSIONS).toHaveLength(34);
    expect(Object.keys(ROLE_PERMISSIONS).sort()).toEqual([...ROLES].sort());
  });

  const cells = APPENDIX_D.flatMap(([permission, member, admin, owner]) => {
    const held = { member, admin, owner, support: admin } satisfies Record<PermissionRole, boolean>;
    return ROLES.map((role) => ({ permission, role, held: held[role] }));
  });

  it.each(cells)("$permission for $role: $held", ({ permission, role, held }) => {
    expect(can(role, permission)).toBe(held);
  });
});

type Need = "operate" | "manage" | "own";

// decideAccess as trunk (a51a30280) wrote it, before it answered through `can`.
const trunkKind = (role: Role | null, isStaff: boolean, supportAllowed: boolean, need: Need) => {
  if (role !== null) {
    if (need === "manage" && role === "member") return "forbidden";
    if (need === "own" && role !== "owner") return "forbidden";
    return "member";
  }
  if (isStaff && supportAllowed) {
    if (need === "own") return "forbidden";
    return "support";
  }
  return "not_found";
};

describe("decideAccess through can", () => {
  const organization = { id: "00000000-0000-4000-8000-000000000001", slug: "acme" };
  const needs: ReadonlyArray<Need> = ["operate", "manage", "own"];
  const roles: ReadonlyArray<Role | null> = ["owner", "admin", "member", null];

  it("returns what trunk returned for every role, need and support setting", () => {
    for (const need of needs) {
      expect(decideAccess({ userId: "user-1", facts: null, isStaff: true, need })).toEqual({ kind: "not_found" });
      for (const role of roles) {
        for (const isStaff of [false, true]) {
          for (const supportAllowed of [false, true]) {
            const facts = { organization, role, supportAllowed, supportShows: SUPPORT_SHOWS_NONE };
            const decision = decideAccess({ userId: "user-1", facts, isStaff, need });
            expect({ role, isStaff, supportAllowed, need, kind: decision.kind }).toEqual({
              role,
              isStaff,
              supportAllowed,
              need,
              kind: trunkKind(role, isStaff, supportAllowed, need),
            });
            if (decision.kind === "forbidden") expect(decision.need).toBe(need);
          }
        }
      }
    }
  });
});
