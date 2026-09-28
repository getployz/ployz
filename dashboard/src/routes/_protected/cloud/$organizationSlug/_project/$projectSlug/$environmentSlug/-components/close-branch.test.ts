import { expect, it } from "vitest";
import { closeDeletes, closeMode } from "./close-branch";

it("closes at once while an open pull request would bring it back, asks once when nothing real goes, else asks typed", () => {
  const plain = { kept: false, hasBranches: false, isDefault: false, prOpen: false };
  expect(closeMode(plain)).toBe("confirm");
  expect(closeMode({ ...plain, prOpen: true })).toBe("now");
  expect(closeMode({ ...plain, kept: true, prOpen: true })).toBe("typed");
  expect(closeMode({ ...plain, hasBranches: true })).toBe("typed");
  expect(closeMode({ ...plain, isDefault: true })).toBe("typed");
  expect(closeDeletes([])).toBe("Nothing runs here yet.");
  expect(closeDeletes(["web", "api", "pg-data"])).toBe("Deletes its own copies of web, api and pg-data.");
});
