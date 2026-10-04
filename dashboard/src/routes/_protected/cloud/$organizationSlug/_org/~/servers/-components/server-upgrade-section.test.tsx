// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { serverUpgradeLine, type LatestUpgrade } from "#/modules/server-upgrade/server-upgrade";
import { ServerUpgradeRow } from "./server-upgrade-section";

afterEach(cleanup);

const failed = (overrides: Partial<LatestUpgrade> = {}): LatestUpgrade => ({
  attemptId: "b".repeat(32), outcome: "failed", stage: "readiness", error: "readiness timed out; restored 0.2.1",
  fromVersion: "0.2.1", targetVersion: "0.2.2", startedAt: new Date().toISOString(), ...overrides,
});
const row = (input: { latest?: LatestUpgrade | null; status?: "online" | "offline"; pendingFrom?: string | null }) => {
  const onUpgrade = vi.fn();
  const line = serverUpgradeLine({
    version: "0.2.1", status: input.status ?? "online", release: "0.2.2", latest: input.latest ?? null,
    pendingFrom: input.pendingFrom, now: Date.now(),
  });
  render(<ServerUpgradeRow version="0.2.1" line={line} onUpgrade={onUpgrade} />);
  return onUpgrade;
};
const description = () => screen.getByText("0.2.1").parentElement?.textContent;

it("shows the version and offers the release", () => {
  const onUpgrade = row({});
  expect(description()).toContain("0.2.2 is out");
  fireEvent.click(screen.getByRole("button", { name: "Upgrade" }));
  expect(onUpgrade).toHaveBeenCalledTimes(1);
});

it("reads Upgrading to the release once clicked, with no button", () => {
  row({ pendingFrom: null });
  expect(description()).toContain("Upgrading to 0.2.2");
  expect(screen.queryByRole("button")).toBeNull();
});

it("says a failure changed nothing, reveals the exact error with Copy, and tries again", () => {
  const onUpgrade = row({ latest: failed() });
  expect(description()).toContain("0.2.2 didn’t install. Nothing changed, and we’re on it.");

  fireEvent.click(screen.getByRole("button", { name: "Show details" }));
  expect(screen.getByText("readiness timed out; restored 0.2.1")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Copy" })).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Try again" }));
  expect(onUpgrade).toHaveBeenCalledTimes(1);
});

it("drops Nothing changed and Try again while the Server is offline, and shows the stage for unknown", () => {
  row({ latest: failed({ outcome: "unknown", error: null, stage: "restarting" }), status: "offline" });
  expect(description()).toContain("0.2.2 didn’t install, and we’re on it.");
  expect(screen.queryByRole("button", { name: "Try again" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Show details" }));
  expect(screen.getByText("restarting")).toBeTruthy();
});
