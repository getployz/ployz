import { describe, expect, it } from "vitest";
import { admit, beyondEnvironments, type Lands, refusalOf, type Role } from "#/modules/organization/access.server";
import { presetGrant, type TokenGrant } from "#/modules/identity/token-grant";
import { PERMISSIONS, type Permission, permissionsOf } from "#/modules/organization/permissions";
import { SUPPORT_SHOWS_NONE } from "#/modules/support-access/support-access";

/** Support holds what admin does, except the permissions named here as withheld from it. */
const WITHHELD_FROM_SUPPORT: ReadonlyArray<Permission> = ["environments:protect"];

const GRANTS: ReadonlyArray<readonly [permission: Permission, member: boolean, admin: boolean, owner: boolean]> = [
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

const organization = { id: "00000000-0000-4000-8000-000000000001", slug: "acme" };
const userId = "user-1";

const admitted = (role: Role | null, permission: Permission, support = { isStaff: false, allowed: false }, grant?: TokenGrant, lands?: Lands) =>
  admit({
    userId,
    facts: { organization, role, supportAllowed: support.allowed, supportShows: SUPPORT_SHOWS_NONE },
    isStaff: support.isStaff,
    permission,
    grant,
    lands,
  });

describe("admit", () => {
  it("covers the 34 permissions in order", () => {
    expect(PERMISSIONS).toEqual(GRANTS.map(([permission]) => permission));
    expect(PERMISSIONS).toHaveLength(34);
  });

  const cells = GRANTS.flatMap(([permission, member, admin, owner]) => [
    { permission, role: "member" as const, held: member },
    { permission, role: "admin" as const, held: admin },
    { permission, role: "owner" as const, held: owner },
  ]);

  it.each(cells)("$permission for $role: $held", ({ permission, role, held }) => {
    expect(admitted(role, permission)).toEqual(held
      ? { kind: "member", userId, role, organization, permissions: permissionsOf(role), environments: undefined }
      : { kind: "forbidden", permission, message: refusalOf(permission) });
  });

  it.each(GRANTS.map(([permission, , admin]) => ({ permission, held: admin && !WITHHELD_FROM_SUPPORT.includes(permission) })))(
    "$permission for support while allowed: $held",
    ({ permission, held }) => {
      expect(admitted(null, permission, { isStaff: true, allowed: true })).toEqual(held
        ? { kind: "support", userId, organization, shows: SUPPORT_SHOWS_NONE, permissions: permissionsOf("support"), environments: undefined }
        : { kind: "forbidden", permission, message: refusalOf(permission) });
    },
  );

  it.each(["billing", "constructor", "__proto__"])("forbids a stored role outside the three (%s) without throwing", (stored) => {
    for (const permission of PERMISSIONS) {
      expect(admitted(stored as Role, permission)).toEqual({ kind: "forbidden", permission, message: refusalOf(permission) });
    }
  });

  it("tells outsiders, staff in an Organization that doesn't allow support, and a missing Organization nothing", () => {
    for (const permission of PERMISSIONS) {
      expect(admitted(null, permission)).toEqual({ kind: "not_found" });
      expect(admitted(null, permission, { isStaff: true, allowed: false })).toEqual({ kind: "not_found" });
      expect(admitted(null, permission, { isStaff: false, allowed: true })).toEqual({ kind: "not_found" });
      expect(admit({ userId, facts: null, isStaff: true, permission })).toEqual({ kind: "not_found" });
    }
  });
});

describe("admit with a token's grant", () => {
  const none = { isStaff: false, allowed: false };

  it("holds what the grant leaves of the role, and refuses the rest by the grant", () => {
    const read = presetGrant({ preset: "read" });
    expect(admitted("owner", "changes:deploy", none, read)).toEqual({ kind: "forbidden", permission: "changes:deploy", message: "This token's grant doesn't include changes:deploy." });
    expect(admitted("member", "servers:shell", none, read)).toEqual({ kind: "forbidden", permission: "servers:shell", message: refusalOf("servers:shell") });
    const held = admitted("owner", "logs:read", none, read);
    expect(held).toMatchObject({ kind: "member", environments: undefined });
    expect("permissions" in held && held.permissions.every((permission) => permission.endsWith(":read"))).toBe(true);
  });

  it("carries a Deploy token's Environment to a Store write, which the Store holds to it", () => {
    expect(admitted("owner", "changes:deploy", none, presetGrant({ preset: "deploy", environment: "staging" }), "store"))
      .toMatchObject({ kind: "member", environments: ["staging"] });
  });

  it("refuses a token held to Environments a write that lands in the Organization or another Environment", () => {
    const scoped: TokenGrant = { permissions: ["servers:operate", "changes:deploy", "logs:read"], environments: ["staging"] };
    const outside = (permission: Permission) => ({ kind: "forbidden", permission, message: "This token can deploy only to staging." });
    expect(admitted("owner", "servers:operate", none, scoped)).toEqual(outside("servers:operate"));
    expect(admitted("owner", "changes:deploy", none, scoped, { environment: "production" })).toEqual(outside("changes:deploy"));
    expect(admitted("owner", "changes:deploy", none, scoped, { environment: "staging" })).toMatchObject({ kind: "member" });
    expect(admitted("owner", "logs:read", none, scoped)).toMatchObject({ kind: "member" });
    expect(admitted("owner", "servers:operate", none, presetGrant({ preset: "full" }))).toMatchObject({ kind: "member" });
  });

  it("lets anything without Environments land anywhere", () => {
    for (const lands of ["reads", "store", "organization", { environment: "production" }] as const) {
      expect(beyondEnvironments(undefined, lands)).toBeNull();
    }
    expect(beyondEnvironments(["staging"], "reads")).toBeNull();
    expect(beyondEnvironments(["staging"], "organization")).toBe("This token can deploy only to staging.");
  });
});

describe("refusalOf", () => {
  it("names owners and admins for an admin permission and owners alone for an owner one", () => {
    for (const [permission, , admin] of GRANTS) {
      expect(refusalOf(permission)).toBe(admin
        ? "Only an owner or admin of this Organization can do this."
        : "Only an owner of this Organization can do this.");
    }
  });
});
