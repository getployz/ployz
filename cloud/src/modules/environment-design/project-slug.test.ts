import { describe, expect, it } from "vitest";
import { projectBaseSlug } from "#/modules/environment-design/workspace-schemas";

describe("projectBaseSlug", () => {
  it("slugifies a trimmed project name", () => {
    expect(projectBaseSlug("  My Project  ")).toBe("my-project");
  });

  it("falls back when the name has no slug characters", () => {
    expect(projectBaseSlug("???")).toBe("project");
  });
});
