// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { serversUpgradeLine, type LatestUpgrade } from "#/modules/server-upgrade/server-upgrade";
import { ServersUpgradeText } from "./servers-upgrade-line";

afterEach(cleanup);

const running: LatestUpgrade = {
  attemptId: "b".repeat(32), outcome: "running", stage: null, error: null,
  fromVersion: "0.2.1", targetVersion: "0.2.2", startedAt: new Date().toISOString(),
};
const show = (versions: string[], input: { latest?: LatestUpgrade[]; lastUpgradedAt?: string | null } = {}) => {
  const onUpgrade = vi.fn();
  const line = serversUpgradeLine({
    servers: versions.map((version) => ({ version })), release: "0.2.2", latest: input.latest ?? [],
    lastUpgradedAt: input.lastUpgradedAt ?? null, pendingFrom: undefined, now: Date.now(),
  });
  const { container } = render(<ServersUpgradeText line={line} onUpgrade={onUpgrade} />);
  return { text: container.textContent, onUpgrade };
};

it("says which release every Server runs, and when they last upgraded", () => {
  expect(show(["0.2.2", "0.2.2"]).text).toBe("On Ployz 0.2.2");
  cleanup();
  expect(show(["0.2.2"], { lastUpgradedAt: new Date(Date.now() - 3 * 3_600_000).toISOString() }).text)
    .toBe("On Ployz 0.2.2, upgraded 3 hours ago");
  expect(screen.queryByRole("button")).toBeNull();
});

it("shows a running rollout's progress", () => {
  expect(show(["0.2.2", "0.2.1", "0.2.1", "0.2.1"], { latest: [running] }).text).toBe("Upgrading to 0.2.2 · 1 of 4 done");
  expect(screen.queryByRole("button")).toBeNull();
});

it("offers Upgrade while every Server is behind", () => {
  const { text, onUpgrade } = show(["0.2.1", "0.2.1"]);
  expect(text).toBe("Ployz 0.2.2 is out · your servers run 0.2.1Upgrade");
  fireEvent.click(screen.getByRole("button", { name: "Upgrade" }));
  expect(onUpgrade).toHaveBeenCalledTimes(1);
});

it("offers Upgrade the rest once some Servers have upgraded", () => {
  const { text, onUpgrade } = show(["0.2.2", "0.2.1", "0.2.1", "0.2.1"]);
  expect(text).toBe("Ployz 0.2.2 is out · 1 of 4 upgradedUpgrade the rest");
  fireEvent.click(screen.getByRole("button", { name: "Upgrade the rest" }));
  expect(onUpgrade).toHaveBeenCalledTimes(1);
});
