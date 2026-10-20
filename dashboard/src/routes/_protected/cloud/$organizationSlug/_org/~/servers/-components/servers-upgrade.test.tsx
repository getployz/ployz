// @vitest-environment jsdom
import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { newMajorLine, type ServersUpgradeLine } from "#/modules/server-upgrade/server-upgrade";
import * as upgradeHooks from "#/modules/server-upgrade/server-upgrade.hooks";
import { UPGRADE_REFUSED } from "#/modules/server-upgrade/server-upgrade.hooks";
import { clientWithRole, expectGate, ROLE_STATES } from "#/test/your-role";
import { ServersUpgradeActions, ServersUpgradeBar } from "./servers-upgrade";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const bar = (line: ServersUpgradeLine, input: { major?: ReturnType<typeof newMajorLine>; may?: boolean | undefined } = {}) => {
  const upgrade = vi.fn();
  const onSettings = vi.fn();
  const { container } = render(
    <ServersUpgradeActions line={line} major={input.major ?? null} may={"may" in input ? input.may : true} upgrade={upgrade} onSettings={onSettings} />,
  );
  return { text: container.textContent, upgrade, onSettings };
};

it("shows only the upgrade settings while the Servers are current, or wait to come back", () => {
  const { text, onSettings } = bar({ kind: "current", release: "0.2.2" });
  expect(text).toBe("");
  fireEvent.click(screen.getByRole("button", { name: "Upgrade settings" }));
  expect(onSettings).toHaveBeenCalledTimes(1);
  cleanup();
  expect(bar({ kind: "when-back", release: "0.2.2" }).text).toBe("");
  cleanup();
  expect(bar(null).text).toBe("");
  expect(screen.getAllByRole("button").map((button) => button.getAttribute("aria-label"))).toEqual(["Upgrade settings"]);
});

it("offers Upgrade to the release while Servers are behind", () => {
  const { upgrade } = bar({ kind: "behind", release: "0.2.3", canUpgrade: true });
  fireEvent.click(screen.getByRole("button", { name: "Upgrade to 0.2.3" }));
  expect(upgrade).toHaveBeenCalledTimes(1);
});

it("lets only a role that may upgrade press Upgrade, and says who may once your role is known", () => {
  const upgradeTo = () => screen.getByRole<HTMLButtonElement>("button", { name: "Upgrade to 0.2.3" });
  const behind = { kind: "behind", release: "0.2.3", canUpgrade: true } as const;
  bar(behind, { may: false });
  expect([upgradeTo().disabled, upgradeTo().title]).toEqual([true, UPGRADE_REFUSED]);
  cleanup();
  bar(behind, { may: undefined });
  expect([upgradeTo().disabled, upgradeTo().getAttribute("title")]).toEqual([true, null]);
  cleanup();
  bar(behind, { may: true });
  expect([upgradeTo().disabled, upgradeTo().getAttribute("title")]).toEqual([false, null]);
});

it("offers no Upgrade when no Server behind can take one", () => {
  expect(bar({ kind: "behind", release: "0.2.3", canUpgrade: false }).text).toBe("");
});

it("shows a running rollout's progress as a count instead of Upgrade", () => {
  expect(bar({ kind: "upgrading", target: "0.2.3", done: 1, total: 3 }).text).toBe("Upgrading 1/3");
  expect(screen.queryByRole("button", { name: /^Upgrade to/ })).toBeNull();
});

it("links a newer major line's release notes, and nothing for the Servers' own line", () => {
  bar({ kind: "current", release: "0.2.2" }, { major: newMajorLine("v0", "1.0.0") });
  expect(screen.getByRole("link", { name: "Ployz 1.0 is out" }).getAttribute("href")).toBe("https://github.com/getployz/ployz/releases/tag/v1.0.0");
  cleanup();
  bar({ kind: "current", release: "0.2.2" }, { major: newMajorLine("v0", "0.3.0") });
  expect(screen.queryByRole("link")).toBeNull();
});

it.each(ROLE_STATES)("opens Upgrade to the roles Cloud lets upgrade, support's included (%s)", async (state) => {
  const client = await clientWithRole("ada", state);
  vi.spyOn(upgradeHooks, "useServerUpgradeSettings").mockReturnValue({ automatic: false, setAutomatic: vi.fn(), channel: "stable", setChannel: vi.fn() });
  const upgrade = { line: { kind: "behind", release: "0.2.3", canUpgrade: true } as const, rows: new Map(), major: null, upgrade: vi.fn() };
  render(<QueryClientProvider client={client}><ServersUpgradeBar organizationSlug="ada" upgrade={upgrade} /></QueryClientProvider>);
  expectGate(screen.getByRole("button", { name: "Upgrade to 0.2.3" }), state, "servers:operate", UPGRADE_REFUSED);
});
