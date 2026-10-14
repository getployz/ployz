import { describe, expect, it } from "vitest";
import { decideAccess, type Role } from "#/modules/organization/access.server";
import { SUPPORT_SHOWS_NONE } from "#/modules/support-access/support-access";

const organization = { id: "00000000-0000-4000-8000-000000000001", slug: "acme" };
const userId = "user-1";
const decide = (role: Role | null, supportAllowed: boolean, isStaff: boolean, need: "operate" | "manage" | "own" = "operate") =>
  decideAccess({ userId, facts: { organization, role, supportAllowed, supportShows: SUPPORT_SHOWS_NONE }, isStaff, need }).kind;

describe("decideAccess", () => {
  it("lets members in by role, staff only while support is allowed, and never outsiders", () => {
    expect(decide("owner", false, false)).toBe("member");
    expect(decide(null, true, true)).toBe("support");
    expect(decide(null, false, true)).toBe("not_found");
    expect(decide(null, true, false)).toBe("not_found");
  });

  it("lets only owners own the Organization, never admins, members or support", () => {
    expect(decide("owner", false, false, "own")).toBe("member");
    expect(decide("admin", false, false, "own")).toBe("forbidden");
    expect(decide("member", false, false, "own")).toBe("forbidden");
    expect(decide(null, true, true, "own")).toBe("forbidden");
    expect(decide(null, false, false, "own")).toBe("not_found");
  });

  it("lets owners, admins and support manage, not members", () => {
    expect(decide("owner", false, false, "manage")).toBe("member");
    expect(decide("admin", false, false, "manage")).toBe("member");
    expect(decide("member", false, false, "manage")).toBe("forbidden");
    expect(decide(null, true, true, "manage")).toBe("support");
  });
});
