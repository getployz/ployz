// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { newMajorLine, type ServersUpgradeLine, type ServerUpgradeLine } from "#/modules/server-upgrade/server-upgrade";
import { ServersUpgradeActions, ServerUpgradeTag } from "./servers-upgrade";

afterEach(cleanup);

const bar = (line: ServersUpgradeLine, input: { automatic?: boolean; major?: ReturnType<typeof newMajorLine> } = {}) => {
  const upgrade = vi.fn();
  const onSettings = vi.fn();
  const { container } = render(
    <ServersUpgradeActions line={line} major={input.major ?? null} automatic={input.automatic ?? true} upgrade={upgrade} onSettings={onSettings} />,
  );
  return { text: container.textContent, upgrade, onSettings };
};
const tag = (line: ServerUpgradeLine) => render(<ServerUpgradeTag line={line} />).container.textContent;

it("shows only the upgrade settings while the Servers are current, or wait to come back", () => {
  const { text, onSettings } = bar({ kind: "current", release: "0.2.2" });
  expect(text).toBe("Upgrades: Automatic");
  fireEvent.click(screen.getByRole("button", { name: "Upgrades: Automatic" }));
  expect(onSettings).toHaveBeenCalledTimes(1);
  cleanup();
  expect(bar({ kind: "when-back", release: "0.2.2" }, { automatic: false }).text).toBe("Upgrades: Manual");
  cleanup();
  expect(bar(null).text).toBe("Upgrades: Automatic");
});

it("offers Upgrade to the release while Servers are behind", () => {
  const { upgrade } = bar({ kind: "behind", release: "0.2.3", upgraded: 0, canUpgrade: true });
  fireEvent.click(screen.getByRole("button", { name: "Upgrade to 0.2.3" }));
  expect(upgrade).toHaveBeenCalledTimes(1);
});

it("offers Upgrade the rest once some Servers have upgraded", () => {
  const { upgrade } = bar({ kind: "behind", release: "0.2.3", upgraded: 1, canUpgrade: true });
  fireEvent.click(screen.getByRole("button", { name: "Upgrade the rest" }));
  expect(upgrade).toHaveBeenCalledTimes(1);
});

it("offers no Upgrade when no Server behind can take one", () => {
  expect(bar({ kind: "behind", release: "0.2.3", upgraded: 1, canUpgrade: false }, { automatic: false }).text).toBe("Upgrades: Manual");
});

it("shows a running rollout's progress instead of Upgrade", () => {
  expect(bar({ kind: "upgrading", target: "0.2.3", done: 1, total: 3 }).text).toBe("Upgrading to 0.2.3 · 1 of 3Upgrades: Automatic");
  expect(screen.queryByRole("button", { name: /^Upgrade (to|the rest)/ })).toBeNull();
});

it("links a newer major line's release notes, and nothing for the Servers' own line", () => {
  bar({ kind: "current", release: "0.2.2" }, { major: newMajorLine("v0", "1.0.0") });
  expect(screen.getByRole("link", { name: "Ployz 1.0 is out" }).getAttribute("href")).toBe("https://github.com/getployz/ployz/releases/tag/v1.0.0");
  cleanup();
  bar({ kind: "current", release: "0.2.2" }, { major: newMajorLine("v0", "0.3.0") });
  expect(screen.queryByRole("link")).toBeNull();
});

it("tags each row with its own upgrade state", () => {
  expect(tag({ kind: "upgrading", target: "0.2.3" })).toBe("Upgrading");
  cleanup();
  expect(tag({ kind: "failed", target: "0.2.3", nothingChanged: true, details: "boom", canRetry: true })).toBe("Upgrade failed");
  cleanup();
  expect(tag({ kind: "behind", release: "0.2.3", canUpgrade: true })).toBe("→ 0.2.3");
  cleanup();
  expect(tag({ kind: "when-back", release: "0.2.3" })).toBe("→ 0.2.3 when back");
  cleanup();
  expect(tag(null)).toBe("");
});
