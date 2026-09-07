import { describe, expect, it } from "vitest";
import { getKindState } from "./diff-kind-state";

describe("getKindState", () => {
  it("maps deployment diff kinds to surface states", () => {
    expect(getKindState("add")).toBe("success");
    expect(getKindState("update")).toBe("changed");
    expect(getKindState("remove")).toBe("destructive");
  });
});
