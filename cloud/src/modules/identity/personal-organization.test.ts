import { describe, expect, it } from "vitest";
import {
  personalOrganizationBaseSlug,
  personalOrganizationName,
} from "#/modules/environment-design/workspace-schemas";

describe("personalOrganizationName", () => {
  it("uses the trimmed display name", () => {
    expect(
      personalOrganizationName({
        id: "user-1",
        name: " Jerry ",
        email: "jerry@example.com",
      }),
    ).toBe("Jerry's Projects");
  });

  it("falls back to the email local part when the name is empty", () => {
    expect(
      personalOrganizationName({
        id: "user-1",
        name: "  ",
        email: "jerry@example.com",
      }),
    ).toBe("jerry's Projects");
  });
});

describe("personalOrganizationBaseSlug", () => {
  it("uses the first name", () => {
    expect(
      personalOrganizationBaseSlug({
        id: "user-1",
        name: "Jerry Seinfeld",
        email: "jerry@example.com",
      }),
    ).toBe("jerry");
  });

  it("falls back to the email local part when the name has no slug characters", () => {
    expect(
      personalOrganizationBaseSlug({
        id: "user-1",
        name: "???",
        email: "jerry@example.com",
      }),
    ).toBe("jerry");
  });
});
