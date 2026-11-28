// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { TrashList } from "@ployz/sdk";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as members from "#/modules/organization/members.queries";
import * as approval from "#/modules/trash/trash-approval";
import * as functions from "#/modules/trash/trash.functions";
import { ServerTrashSection } from "./server-trash-section";

const server = { id: "1".repeat(32), name: "web-1" };
const first = "a".repeat(26);
const second = "b".repeat(26);
const listed: TrashList = {
  entries: [
    { id: first, place: "root", name: "shop-production_vol-pg", trashed_at: 1_795_000_000, expires_at: 1_795_604_800, used_bytes: 2_400_000_000 },
    { id: second, place: "slot", name: "shop-production_vol-uploads", trashed_at: 1_795_000_000, expires_at: 1_795_604_800, used_bytes: 51_000_000 },
  ],
  unreadable: [{ id: "c".repeat(26), used_bytes: 1_000, reason: "its marks are missing" }],
};

beforeEach(() => {
  vi.spyOn(members, "useRoleGate").mockReturnValue({ allowed: true, reason: undefined });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function renderTrash(trash: Awaited<ReturnType<typeof functions.listServerTrashServerFn>>) {
  vi.spyOn(functions, "listServerTrashServerFn").mockResolvedValue(trash);
  render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <ServerTrashSection organizationSlug="ada" server={server} />
    </QueryClientProvider>,
  );
}

it("lists each entry with what it is, its size and when it was deleted, and an entry Ployz can't read", async () => {
  renderTrash({ kind: "listed", trash: listed });
  expect(await screen.findByText("shop-production_vol-pg")).toBeTruthy();
  const pg = screen.getByText("shop-production_vol-pg").closest("[role=listitem]");
  expect(pg?.textContent).toMatch(/Volume · 2\.4 GB · Deleted .+ · Kept until .+/);
  expect(screen.getByText("shop-production_vol-uploads").closest("[role=listitem]")?.textContent).toContain("Mirror or old copy · 51 MB");
  expect(screen.getByText(/^Unreadable · /)).toBeTruthy();
  expect(screen.queryByRole("checkbox", { name: new RegExp("c".repeat(26)) })).toBeNull();
});

it("restores only the picked entries, with a passkey approval of exactly those on this server", async () => {
  const approve = vi.spyOn(approval, "approvedTrash").mockResolvedValue("signed");
  const restoreFn = vi.spyOn(functions, "restoreServerTrashServerFn").mockResolvedValue({ restored: [], failed: [] });
  renderTrash({ kind: "listed", trash: listed });
  const restore = await screen.findByRole("button", { name: "Restore" });
  expect(restore.hasAttribute("disabled")).toBe(true);

  fireEvent.click(screen.getByRole("checkbox", { name: /shop-production_vol-uploads/ }));
  fireEvent.click(screen.getByRole("button", { name: "Restore" }));

  await waitFor(() => expect(restoreFn).toHaveBeenCalled());
  expect(approve).toHaveBeenCalledWith(expect.anything(), "ada", server.id, "restore", [second]);
  expect(restoreFn).toHaveBeenCalledWith({ data: { organizationSlug: "ada", machineId: server.id, approval: "signed" } });
});

it("sends nothing when the passkey doesn't answer, and says so", async () => {
  vi.spyOn(approval, "approvedTrash").mockRejectedValue(new approval.TrashNotApproved("Your passkey didn't answer. Try again."));
  const restoreFn = vi.spyOn(functions, "restoreServerTrashServerFn");
  renderTrash({ kind: "listed", trash: listed });
  fireEvent.click(await screen.findByRole("checkbox", { name: /shop-production_vol-pg/ }));
  fireEvent.click(screen.getByRole("button", { name: "Restore" }));
  expect((await screen.findByRole("alert")).textContent).toBe("Your passkey didn't answer. Try again.");
  expect(restoreFn).not.toHaveBeenCalled();
});

it("deletes the picked entries only once the server's name is typed, approving exactly those", async () => {
  const approve = vi.spyOn(approval, "approvedTrash").mockResolvedValue("signed");
  const emptyFn = vi.spyOn(functions, "emptyServerTrashServerFn").mockResolvedValue({ emptied: [first], missing: [], failed: [], restoring: [] });
  renderTrash({ kind: "listed", trash: listed });
  fireEvent.click(await screen.findByRole("checkbox", { name: /shop-production_vol-pg/ }));
  fireEvent.click(screen.getByRole("button", { name: "Delete" }));

  const dialog = await screen.findByRole("alertdialog", { name: "Delete shop-production_vol-pg?" });
  expect(dialog.textContent).toContain("shop-production_vol-pg");
  expect(dialog.textContent).not.toContain("shop-production_vol-uploads");
  const confirm = within(dialog).getByRole("button", { name: "Delete" });
  expect(confirm.hasAttribute("disabled")).toBe(true);
  fireEvent.change(within(dialog).getByRole("textbox", { name: "Type web-1 to confirm" }), { target: { value: "web-1" } });
  await waitFor(() => expect(confirm.hasAttribute("disabled")).toBe(false));
  fireEvent.click(confirm);

  await waitFor(() => expect(emptyFn).toHaveBeenCalled());
  expect(approve).toHaveBeenCalledWith(expect.anything(), "ada", server.id, "empty", [first]);
  expect(emptyFn).toHaveBeenCalledWith({ data: { organizationSlug: "ada", machineId: server.id, approval: "signed" } });
});

it("says why a server's trash couldn't be read", async () => {
  renderTrash({ kind: "unread", message: "web-1 runs an older Ployz that shows its Trash only on the Server. Upgrade it to see its Trash here." });
  expect(await screen.findByText(/Upgrade it to see its Trash here/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Restore" })).toBeNull();
});
