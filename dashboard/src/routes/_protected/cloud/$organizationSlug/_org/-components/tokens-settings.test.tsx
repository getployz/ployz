// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { TokenItem } from "./tokens-settings";

afterEach(cleanup);

const token = {
  name: "ci-deploy",
  scope: "Deploy staging",
  expires_at: "2027-01-01T00:00:00Z",
  expired: false,
};

it("asks before revoking a token, and revokes only on confirm", async () => {
  const onRevoke = vi.fn();
  render(<TokenItem token={token} revoking={false} onRevoke={onRevoke} />);
  fireEvent.click(screen.getByRole("button", { name: "Revoke" }));

  const dialog = await screen.findByRole("alertdialog", { name: "Revoke ci-deploy?" });
  expect(within(dialog).getByText("Anything using it stops working.")).toBeTruthy();
  expect(onRevoke).not.toHaveBeenCalled();

  fireEvent.click(within(dialog).getByRole("button", { name: "Revoke" }));
  expect(onRevoke).toHaveBeenCalledOnce();
});

it("keeps the token when the confirm is cancelled", async () => {
  const onRevoke = vi.fn();
  render(<TokenItem token={token} revoking={false} onRevoke={onRevoke} />);
  fireEvent.click(screen.getByRole("button", { name: "Revoke" }));

  fireEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Cancel" }));
  expect(onRevoke).not.toHaveBeenCalled();
});
