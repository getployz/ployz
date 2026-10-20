// @vitest-environment jsdom
import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { Dialog, DialogContent } from "#/components/ui/dialog";
import * as upgradeHooks from "#/modules/server-upgrade/server-upgrade.hooks";
import { ROLE_CHECKING } from "#/modules/organization/members.queries";
import { clientWithRole, gateReason, ROLE_STATES } from "#/test/your-role";
import { ServerUpgradesDialog, ServerUpgradesForm } from "./server-upgrades-dialog";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it.each([
  { gate: { allowed: false, reason: "Only an owner or admin can change these." } },
  { gate: { allowed: false, reason: ROLE_CHECKING } },
  { gate: { allowed: true, reason: undefined } },
])("lets only a role that may upgrade change Server upgrades, and says why not ($gate.reason)", ({ gate }) => {
  const disabled = !gate.allowed;
  render(
    <Dialog open>
      <DialogContent>
        <ServerUpgradesForm automatic={false} setAutomatic={vi.fn()} channel="stable" setChannel={vi.fn()} gate={gate} />
      </DialogContent>
    </Dialog>,
  );
  expect(screen.getByRole("switch", { name: "Upgrade automatically" }).hasAttribute("data-disabled")).toBe(disabled);
  expect(screen.getByRole("combobox", { name: "Releases" }).hasAttribute("data-disabled")).toBe(disabled);
  if (gate.reason === undefined) expect(screen.queryByText(/Only an owner|your role/)).toBeNull();
  else expect(screen.getByRole("dialog", { description: gate.reason })).toBeTruthy();
});

it.each(ROLE_STATES)("opens Server upgrades to the roles Cloud lets upgrade, support's included (%s)", async (state) => {
  const client = await clientWithRole("ada", state);
  vi.spyOn(upgradeHooks, "useServerUpgradeSettings").mockReturnValue({ automatic: false, setAutomatic: vi.fn(), channel: "stable", setChannel: vi.fn() });
  render(<QueryClientProvider client={client}><ServerUpgradesDialog organizationSlug="ada" open onOpenChange={vi.fn()} /></QueryClientProvider>);
  const open = state === "owner" || state === "admin" || state === "support";
  expect(screen.getByRole("switch", { name: "Upgrade automatically" }).hasAttribute("data-disabled")).toBe(!open);
  const reason = gateReason(state, "servers:operate", "Only an owner or admin can change these.");
  if (reason === undefined) expect(screen.queryByText(/Only an owner|your role/)).toBeNull();
  else expect(await screen.findByRole("dialog", { description: reason })).toBeTruthy();
});
