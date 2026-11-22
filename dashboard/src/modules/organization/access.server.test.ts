import { describe, expect, it } from "vitest";
import { admit, beyondEnvironments, type Lands, refusalOf, type Role } from "#/modules/organization/access.server";
import { presetGrant, type TokenGrant } from "#/modules/identity/token-grant";
import { PERMISSIONS, type Permission, permissionsOf } from "#/modules/organization/permissions";
import { SUPPORT_SHOWS_NONE } from "#/modules/support-access/support-access";

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
  const member = (role: Role) => ({ kind: "member", userId, role, organization, permissions: permissionsOf(role), environments: undefined });
  const forbidden = (permission: Permission) => ({ kind: "forbidden", permission, message: refusalOf(permission) });

  it.each(["org:read", "changes:deploy", "tokens:create", "approvals:answer"] as const)("admits every role to %s", (permission) => {
    for (const role of ["member", "admin", "owner"] as const) expect(admitted(role, permission)).toEqual(member(role));
  });

  it.each(["members:manage", "servers:add", "servers:shell", "servers:remove", "github:connect", "environments:protect"] as const)(
    "admits admins and owners to %s but not members",
    (permission) => {
      expect(admitted("member", permission)).toEqual(forbidden(permission));
      expect(admitted("admin", permission)).toEqual(member("admin"));
      expect(admitted("owner", permission)).toEqual(member("owner"));
    },
  );

  it.each(["org:billing", "org:delete"] as const)("admits only owners to %s", (permission) => {
    expect(admitted("member", permission)).toEqual(forbidden(permission));
    expect(admitted("admin", permission)).toEqual(forbidden(permission));
    expect(admitted("owner", permission)).toEqual(member("owner"));
  });

  it("admits support, while allowed, to admin's work but not to owner's or to protecting Environments", () => {
    const allowed = { isStaff: true, allowed: true };
    expect(admitted(null, "servers:shell", allowed)).toEqual({
      kind: "support", userId, organization, shows: SUPPORT_SHOWS_NONE, permissions: permissionsOf("support"), environments: undefined,
    });
    for (const permission of ["environments:protect", "org:billing", "org:delete"] as const) {
      expect(admitted(null, permission, allowed)).toEqual(forbidden(permission));
    }
  });

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
    expect(refusalOf("servers:add")).toBe("Only an owner or admin of this Organization can do this.");
    expect(refusalOf("org:billing")).toBe("Only an owner of this Organization can do this.");
  });
});
