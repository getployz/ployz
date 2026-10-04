// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { ServerStatus } from "#/modules/machines/server-status";
import { newMajorLine, serversUpgradeLine, type LatestUpgrade } from "#/modules/server-upgrade/server-upgrade";
import { NewMajorLineNotice, ServersUpgradeText } from "./servers-upgrade-line";

afterEach(cleanup);

const running: LatestUpgrade = {
  attemptId: "b".repeat(32), outcome: "running", stage: null, error: null,
  fromVersion: "0.2.1", targetVersion: "0.2.2", startedAt: new Date().toISOString(),
};
const show = (versions: string[], input: {
  latest?: LatestUpgrade[];
  lastUpgradedAt?: string | null;
  automatic?: boolean;
  statuses?: ServerStatus[];
} = {}) => {
  const onUpgrade = vi.fn();
  const onSettings = vi.fn();
  const automatic = input.automatic ?? true;
  const line = serversUpgradeLine({
    servers: versions.map((version, at) => ({ name: `web-${at + 1}`, version, status: input.statuses?.[at] ?? "online" })),
    release: "0.2.2", latest: input.latest ?? [], lastUpgradedAt: input.lastUpgradedAt ?? null, pendingFrom: undefined,
    now: Date.now(), automatic,
  });
  const { container } = render(<ServersUpgradeText line={line} automatic={automatic} onUpgrade={onUpgrade} onSettings={onSettings} />);
  return { text: container.textContent, onUpgrade, onSettings };
};

it("says which release every Server runs, when they last upgraded, and opens the settings", () => {
  expect(show(["0.2.2", "0.2.2"]).text).toBe("On Ployz 0.2.2 · Upgrades automatically");
  cleanup();
  expect(show(["0.2.2"], { lastUpgradedAt: new Date(Date.now() - 3 * 3_600_000).toISOString(), automatic: false }).text)
    .toBe("On Ployz 0.2.2, upgraded 3 hours ago · Manual upgrades");
  cleanup();
  const { onSettings } = show(["0.2.2"]);
  fireEvent.click(screen.getByRole("button", { name: "Upgrades automatically" }));
  expect(onSettings).toHaveBeenCalledTimes(1);
});

it("shows a running rollout's progress", () => {
  expect(show(["0.2.2", "0.2.1", "0.2.1", "0.2.1"], { latest: [running] }).text).toBe("Upgrading to 0.2.2 · 1 of 4 done");
  expect(screen.queryByRole("button")).toBeNull();
});

it("offers Upgrade and the Manual settings while every Server is behind", () => {
  const { text, onUpgrade, onSettings } = show(["0.2.1", "0.2.1"], { automatic: false });
  expect(text).toBe("Ployz 0.2.2 is out · your servers run 0.2.1ManualUpgrade");
  fireEvent.click(screen.getByRole("button", { name: "Upgrade" }));
  fireEvent.click(screen.getByRole("button", { name: "Manual" }));
  expect(onUpgrade).toHaveBeenCalledTimes(1);
  expect(onSettings).toHaveBeenCalledTimes(1);
});

it("offers Upgrade the rest once some Servers have upgraded, as a halted automatic rollout reads", () => {
  const { text, onUpgrade } = show(["0.2.2", "0.2.1", "0.2.1", "0.2.1"]);
  expect(text).toBe("Ployz 0.2.2 is out · 1 of 4 upgradedAutomaticUpgrade the rest");
  fireEvent.click(screen.getByRole("button", { name: "Upgrade the rest" }));
  expect(onUpgrade).toHaveBeenCalledTimes(1);
});

it("names the offline Servers that upgrade when they're back, while automatic upgrades are on", () => {
  expect(show(["0.2.2", "0.2.2", "0.2.1"], { statuses: ["online", "online", "offline"] }).text)
    .toBe("On Ployz 0.2.2 · web-3 upgrades when it’s back · Upgrades automatically");
  cleanup();
  expect(show(["0.2.2", "0.2.1", "0.2.1"], { statuses: ["online", "offline", "offline"] }).text)
    .toBe("On Ployz 0.2.2 · web-2 and web-3 upgrade when they’re back · Upgrades automatically");
  cleanup();
  // With automatic upgrades off the release reads as out, with nothing online to Upgrade.
  expect(show(["0.2.2", "0.2.1"], { statuses: ["online", "offline"], automatic: false }).text)
    .toBe("Ployz 0.2.2 is out · 1 of 2 upgradedManual");
});

it("announces a newer major line with its release notes, and nothing for the Servers' own line", () => {
  const { container } = render(<NewMajorLineNotice notice={newMajorLine("v0", "1.0.0")} />);
  expect(container.textContent).toBe(
    "Ployz 1.0 is out.New lines install only when you choose. Your servers stay on 0.x and keep getting its upgrades.See what’s new",
  );
  expect(screen.getByRole("link", { name: "See what’s new" }).getAttribute("href")).toBe("https://github.com/getployz/ployz/releases/tag/v1.0.0");
  cleanup();
  expect(render(<NewMajorLineNotice notice={newMajorLine("v0", "0.3.0")} />).container.textContent).toBe("");
});
