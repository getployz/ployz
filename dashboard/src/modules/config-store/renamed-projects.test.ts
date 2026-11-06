import { expect, it } from "vitest";
import { rememberProjectRename, renamedProject, renamedProjectHref } from "./renamed-projects";

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

const at = (pathname: string, searchStr = "", hash = "") => ({ pathname, searchStr, hash });

it("opens the same page, search and hash under the new name", () => {
  rememberProjectRename("href", "shop", "store");
  expect(renamedProjectHref("href", "shop", at("/cloud/href/shop/production/settings", "?scope=project", "name"), []))
    .toBe("/cloud/href/store/production/settings?scope=project#name");
  expect(renamedProjectHref("href", "shop", at("/cloud/href/shop"), [])).toBe("/cloud/href/store");
});

it("opens a Project that holds the old name again, and names this tab never renamed", () => {
  rememberProjectRename("taken", "shop", "store");
  expect(renamedProjectHref("taken", "shop", at("/cloud/taken/shop/production"), [{ name: "shop" }, { name: "store" }])).toBeUndefined();
  expect(renamedProjectHref("taken", "blog", at("/cloud/taken/blog/production"), [])).toBeUndefined();
});
