import { describe, expect, it } from "vitest";
import { SECURITY_HEADERS } from "./security-headers";

function directives(policy: string): Map<string, string[]> {
  return new Map(
    policy.split(";").map((directive) => {
      const [name = "", ...sources] = directive.trim().split(/\s+/);
      return [name, sources];
    }),
  );
}

describe("security headers", () => {
  it("loads images only from our origin and GitHub avatars, and keeps every page unframeable", () => {
    const policy = directives(SECURITY_HEADERS["content-security-policy"]);

    expect(policy.get("img-src")).toEqual(["'self'", "data:", "blob:", "https://avatars.githubusercontent.com"]);
    expect(policy.get("frame-ancestors")).toEqual(["'none'"]);
    expect(policy.has("default-src")).toBe(false);
  });
});
