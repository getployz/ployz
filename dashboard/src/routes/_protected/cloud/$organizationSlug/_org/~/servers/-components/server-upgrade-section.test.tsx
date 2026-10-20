// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { serverUpgradeLine, type LatestUpgrade } from "#/modules/server-upgrade/server-upgrade";
import { ROLE_CHECKING, type RoleGate } from "#/modules/organization/members.queries";
import { UPGRADE_REFUSED } from "#/modules/server-upgrade/server-upgrade.hooks";
import { ServerUpgradeRow } from "./server-upgrade-section";

afterEach(cleanup);

const failed = (overrides: Partial<LatestUpgrade> = {}): LatestUpgrade => ({
  attemptId: "b".repeat(32), outcome: "failed", stage: "readiness", error: "readiness timed out; restored 0.2.1",
  fromVersion: "0.2.1", targetVersion: "0.2.2", startedAt: new Date().toISOString(), ...overrides,
});
const row = (input: {
  latest?: LatestUpgrade | null;
  status?: "online" | "offline";
  pendingFrom?: string | null;
  automatic?: boolean;
  rolloutRunning?: boolean;
  gate?: RoleGate;
}) => {
  const onUpgrade = vi.fn();
  const line = serverUpgradeLine({
    version: "0.2.1", status: input.status ?? "online", release: "0.2.2", latest: input.latest ?? null,
    pendingFrom: input.pendingFrom, now: Date.now(), automatic: input.automatic ?? true, rolloutRunning: input.rolloutRunning ?? false,
  });
  render(<ServerUpgradeRow version="0.2.1" line={line} gate={input.gate ?? { allowed: true, reason: undefined }} onUpgrade={onUpgrade} />);
  return onUpgrade;
};
const description = () => screen.getByText("0.2.1").parentElement?.textContent;

it("shows the version and offers the release", () => {
  const onUpgrade = row({});
  expect(description()).toContain("0.2.2 is out");
  fireEvent.click(screen.getByRole("button", { name: "Upgrade" }));
  expect(onUpgrade).toHaveBeenCalledTimes(1);
});

it.each([
  { gate: { allowed: false, reason: UPGRADE_REFUSED }, title: UPGRADE_REFUSED },
  { gate: { allowed: false, reason: ROLE_CHECKING }, title: ROLE_CHECKING },
  { gate: { allowed: true, reason: undefined }, title: null },
])("lets only a role that may upgrade press Upgrade or Try again, and says why not ($title)", ({ gate: roleGate, title }) => {
  const disabled = !roleGate.allowed;
  const gate = (name: string) => {
    const button = screen.getByRole<HTMLButtonElement>("button", { name });
    return [button.disabled, button.getAttribute("title")];
  };
  row({ gate: roleGate });
  expect(gate("Upgrade")).toEqual([disabled, title]);
  cleanup();
  row({ gate: roleGate, latest: failed() });
  expect(gate("Try again")).toEqual([disabled, title]);
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

it("says an offline Server behind upgrades when it's back, while automatic upgrades are on", () => {
  row({ status: "offline" });
  expect(description()).toBe("0.2.1 · Upgrades to 0.2.2 when it’s back");
  expect(screen.queryByRole("button")).toBeNull();
});

it("offers neither Upgrade nor Try again while a rollout runs", () => {
  row({ rolloutRunning: true });
  expect(description()).toContain("0.2.2 is out");
  expect(screen.queryByRole("button", { name: "Upgrade" })).toBeNull();
  cleanup();
  row({ rolloutRunning: true, latest: failed() });
  expect(screen.queryByRole("button", { name: "Try again" })).toBeNull();
});
