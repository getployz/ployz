// @vitest-environment jsdom
import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { Dialog, DialogContent } from "#/components/ui/dialog";
import * as upgradeHooks from "#/modules/server-upgrade/server-upgrade.hooks";
import { clientWithRole, ROLE_STATES } from "#/test/your-role";
import { ServerUpgradesDialog, ServerUpgradesForm } from "./server-upgrades-dialog";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it.each([
  { may: false, disabled: true, says: true },
  { may: undefined, disabled: true, says: false },
  { may: true, disabled: false, says: false },
])("lets only a role that may upgrade change Server upgrades (may: $may)", ({ may, disabled, says }) => {
  render(
    <Dialog open>
      <DialogContent>
        <ServerUpgradesForm automatic={false} setAutomatic={vi.fn()} channel="stable" setChannel={vi.fn()} may={may} />
      </DialogContent>
    </Dialog>,
  );
  expect(screen.getByRole("switch", { name: "Upgrade automatically" }).hasAttribute("data-disabled")).toBe(disabled);
  expect(screen.getByRole("combobox", { name: "Releases" }).hasAttribute("data-disabled")).toBe(disabled);
  expect(screen.queryByText("Only an owner or admin can change these.") !== null).toBe(says);
});

it.each(ROLE_STATES)("opens Server upgrades to the roles Cloud lets upgrade, support's included (%s)", async (state) => {
  const client = await clientWithRole("ada", state);
  vi.spyOn(upgradeHooks, "useServerUpgradeSettings").mockReturnValue({ automatic: false, setAutomatic: vi.fn(), channel: "stable", setChannel: vi.fn() });
  render(<QueryClientProvider client={client}><ServerUpgradesDialog organizationSlug="ada" open onOpenChange={vi.fn()} /></QueryClientProvider>);
  const open = state === "owner" || state === "admin" || state === "support";
  expect(screen.getByRole("switch", { name: "Upgrade automatically" }).hasAttribute("data-disabled")).toBe(!open);
  expect(screen.queryByText("Only an owner or admin can change these.") !== null).toBe(state === "member");
});
