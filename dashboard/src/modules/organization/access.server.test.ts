import { describe, expect, it } from "vitest";
import { decideAccess } from "#/modules/organization/access.server";

const organization = { id: "00000000-0000-4000-8000-000000000001", slug: "acme" };
const userId = "user-1";
const decide = (role: "owner" | null, supportAllowed: boolean, isStaff: boolean) =>
  decideAccess({ userId, facts: { organization, role, supportAllowed }, isStaff, need: "operate" }).kind;

describe("decideAccess", () => {
  it("lets members in by role, staff only while support is allowed, and never outsiders", () => {
    expect(decide("owner", false, false)).toBe("member");
    expect(decide(null, true, true)).toBe("support");
    expect(decide(null, false, true)).toBe("not_found");
    expect(decide(null, true, false)).toBe("not_found");
  });
});
