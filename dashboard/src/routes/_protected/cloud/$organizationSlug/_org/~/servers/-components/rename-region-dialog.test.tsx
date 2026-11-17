// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as storeFunctions from "#/modules/config-store/store.functions";
import { RenameRegionDialog } from "./rename-region-dialog";

let write: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  write = vi.spyOn(storeFunctions, "writeStoreServerFn").mockResolvedValue({ ok: true, value: { written: "region_named", key: "default", name: "Falkenstein" } } as never);
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const REGIONS = [{ key: "lhr", name: "London" }, { key: "default", name: "Default" }, { key: "syd", name: "Sydney" }];

function dialog(allowed = true) {
  const queryClient = new QueryClient();
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  render(
    <QueryClientProvider client={queryClient}>
      <RenameRegionDialog
        organizationSlug="acme"
        regions={REGIONS}
        gate={allowed ? { allowed: true, reason: undefined } : { allowed: false, reason: "Only an owner or admin can change how a server runs." }}
      />
    </QueryClientProvider>,
  );
}

it("opens on the default region, says nothing restarts and shows its key for the CLI", async () => {
  dialog();
  fireEvent.click(screen.getByRole("button", { name: "Rename region" }));
  expect((await screen.findByRole("combobox", { name: "Region" }) as HTMLSelectElement).value).toBe("default");
  expect((screen.getByRole("textbox", { name: "Name" }) as HTMLInputElement).value).toBe("Default");
  expect(screen.getByText("Nothing restarts.")).toBeTruthy();
  expect(screen.getByText("--region default")).toBeTruthy();

  fireEvent.change(screen.getByRole("combobox", { name: "Region" }), { target: { value: "syd" } });
  expect((screen.getByRole("textbox", { name: "Name" }) as HTMLInputElement).value).toBe("Sydney");
  expect(screen.getByText("--region syd")).toBeTruthy();
});

it("renames the picked region by its key and closes", async () => {
  dialog();
  fireEvent.click(screen.getByRole("button", { name: "Rename region" }));
  fireEvent.change(await screen.findByRole("textbox", { name: "Name" }), { target: { value: " Falkenstein " } });
  fireEvent.click(screen.getAllByRole("button", { name: "Rename region" }).at(-1) as HTMLElement);
  await waitFor(() => expect(write).toHaveBeenCalledWith(expect.objectContaining({
    data: expect.objectContaining({ command: { command: "name_region", key: "default", name: "Falkenstein" } }),
  })));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
});

it("sends nothing for an empty or unchanged name", async () => {
  dialog();
  fireEvent.click(screen.getByRole("button", { name: "Rename region" }));
  const submit = () => screen.getAllByRole("button", { name: "Rename region" }).at(-1) as HTMLButtonElement;
  await screen.findByRole("textbox", { name: "Name" });
  expect(submit().disabled).toBe(true);
  fireEvent.change(screen.getByRole("textbox", { name: "Name" }), { target: { value: "  " } });
  expect(submit().disabled).toBe(true);
  expect(write).not.toHaveBeenCalled();
});

it("is not offered to a member who can't operate Servers", () => {
  dialog(false);
  const button = screen.getByRole("button", { name: "Rename region" }) as HTMLButtonElement;
  expect(button.disabled).toBe(true);
  expect(button.title).toBe("Only an owner or admin can change how a server runs.");
});
