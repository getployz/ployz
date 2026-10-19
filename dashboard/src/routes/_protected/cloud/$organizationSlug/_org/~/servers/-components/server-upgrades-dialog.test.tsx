// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { Dialog, DialogContent } from "#/components/ui/dialog";
import { ServerUpgradesForm } from "./server-upgrades-dialog";

afterEach(cleanup);

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
