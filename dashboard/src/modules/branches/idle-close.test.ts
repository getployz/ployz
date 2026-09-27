import { expect, it } from "vitest";
import { dueForIdleClose, idleClose } from "./idle-close";

const DAY = 24 * 60 * 60 * 1000;
const now = new Date("2026-09-27T12:00:00Z");
const daysAgo = (days: number) => new Date(now.getTime() - days * DAY);
const branch = (environmentId: string, parentEnvironmentId = "prod", kept = false) => ({ environmentId, parentEnvironmentId, kept });

it("warns from day 5 with the days left and is due at day 7", () => {
  const inputs = (days: number) => ({
    branches: [branch("b")],
    latestAttemptAt: new Map([["b", daysAgo(days)]]),
    defaultEnvironmentIds: new Set<string>(),
  });
  expect(idleClose("b", inputs(4.9), now)).toEqual({ kind: "none" });
  expect(idleClose("b", inputs(5), now)).toEqual({ kind: "warn", daysLeft: 2 });
  expect(idleClose("b", inputs(6.5), now)).toEqual({ kind: "warn", daysLeft: 1 });
  expect(idleClose("b", inputs(7), now)).toEqual({ kind: "due" });
  expect(idleClose("b", inputs(30), now)).toEqual({ kind: "due" });
});

it("never closes a kept, never-deployed, parent or default Branch, nor a root", () => {
  const inputs = {
    branches: [branch("kept", "prod", true), branch("fresh"), branch("parent"), branch("child", "parent"), branch("default")],
    latestAttemptAt: new Map(["prod", "kept", "parent", "child", "default"].map((id) => [id, daysAgo(10)])),
    defaultEnvironmentIds: new Set(["default"]),
  };
  for (const id of ["prod", "kept", "fresh", "parent", "default"]) expect(idleClose(id, inputs, now)).toEqual({ kind: "none" });
  expect(dueForIdleClose(inputs, now)).toEqual(["child"]);
});

it("sweeps exactly the due Branches", () => {
  const inputs = {
    branches: [branch("idle"), branch("warned"), branch("busy"), branch("old", "prod", true)],
    latestAttemptAt: new Map([["idle", daysAgo(8)], ["warned", daysAgo(6)], ["busy", daysAgo(0)], ["old", daysAgo(90)]]),
    defaultEnvironmentIds: new Set<string>(),
  };
  expect(dueForIdleClose(inputs, now)).toEqual(["idle"]);
});
