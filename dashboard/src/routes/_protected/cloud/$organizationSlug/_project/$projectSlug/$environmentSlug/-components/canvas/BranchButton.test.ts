import { expect, it } from "vitest";
import { branchStatus } from "./BranchButton";

const quiet = { changes: 0, updates: 0, saved: false, shutdown: null, closesIn: null };

it("says the first that applies: a failed shutdown, changes to save, updates, Off, closing soon, saved", () => {
  expect(branchStatus(quiet)).toBe("Up to date");
  expect(branchStatus({ ...quiet, saved: true })).toBe("Saved");
  expect(branchStatus({ ...quiet, saved: true, closesIn: 1 })).toBe("Closes in 1 day");
  expect(branchStatus({ ...quiet, closesIn: 2, shutdown: "off" })).toBe("Off");
  expect(branchStatus({ ...quiet, shutdown: "running" })).toBe("Shutting down");
  expect(branchStatus({ ...quiet, shutdown: "off", updates: 1 })).toBe("1 update");
  expect(branchStatus({ ...quiet, updates: 3, changes: 2 })).toBe("2 to save");
  expect(branchStatus({ ...quiet, changes: 2, shutdown: "failed" })).toBe("Shutdown failed");
});
