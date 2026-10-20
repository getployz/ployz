import { describe, expect, it } from "vitest";
import { type Line, saveTitle } from "./EnvironmentHistory";

const variable = (kind: Line["kind"], node: string, name: string): Line =>
  ({ path: `${node}.${name}`, kind, node, name, variable: true, lifecycle: false, before: "", after: "" });

describe("saveTitle", () => {
  it("says Remove when a save only removed variables", () => {
    expect(saveTitle([variable("remove", "nginx", "SOAK_REF"), variable("remove", "nginx", "SOAK_UNI")], 3)).toBe("Remove nginx SOAK_REF and SOAK_UNI");
    expect(saveTitle([variable("remove", "nginx", "SOAK_REF")], 3)).toBe("Remove nginx SOAK_REF");
    expect(saveTitle([variable("remove", "api", "TOKEN"), variable("remove", "web", "TOKEN")], 3)).toBe("Remove TOKEN from api and web");
  });

  it("says Add when a save only added variables", () => {
    expect(saveTitle([variable("add", "nginx", "A"), variable("add", "nginx", "B")], 3)).toBe("Add nginx A and B");
    expect(saveTitle([variable("add", "api", "TOKEN"), variable("add", "web", "TOKEN")], 3)).toBe("Add TOKEN to api and web");
  });

  it("says Update or Change for a mix", () => {
    expect(saveTitle([variable("add", "nginx", "A"), variable("remove", "nginx", "B")], 3)).toBe("Update nginx A and B");
    expect(saveTitle([variable("update", "nginx", "A")], 3)).toBe("Change nginx A");
    expect(saveTitle([variable("update", "api", "TOKEN"), variable("add", "web", "TOKEN")], 3)).toBe("Set TOKEN on api and web");
  });
});
