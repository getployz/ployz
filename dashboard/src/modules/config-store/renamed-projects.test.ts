import { expect, it } from "vitest";
import { rememberProjectRename, renamedProject } from "./renamed-projects";

it("leads every old name of a Project to its current one", () => {
  rememberProjectRename("chain", "shop", "store");
  rememberProjectRename("chain", "store", "market");
  expect(renamedProject("chain", "shop")).toBe("market");
  expect(renamedProject("chain", "store")).toBe("market");
  expect(renamedProject("chain", "market")).toBeUndefined();
  expect(renamedProject("other", "shop")).toBeUndefined();
});

it("leads nowhere from a name a Project took back", () => {
  rememberProjectRename("back", "shop", "store");
  rememberProjectRename("back", "store", "shop");
  expect(renamedProject("back", "shop")).toBeUndefined();
  expect(renamedProject("back", "store")).toBe("shop");
});
