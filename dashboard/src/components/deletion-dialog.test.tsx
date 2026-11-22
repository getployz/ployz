// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DeletionDialog, type DeletionCallbacks } from "./deletion-dialog";
import type { DeletionItem } from "#/modules/config-store/deletion";

afterEach(() => {
  cleanup();
  document.body.replaceChildren();
  document.body.style.removeProperty("overflow");
});

const GB = 1024 ** 3;
const service = (name: string): DeletionItem => ({ kind: "service", name });
const volume = (name: string, gb?: number): DeletionItem => ({ kind: "volume", name, bytes: gb === undefined ? undefined : gb * GB });
const branch = (name: string): DeletionItem => ({ kind: "branch", name });

/** A servers' check whose evidence names how many things it saw. */
const check = (items: DeletionItem[]) => ({ items, evidence: `evidence:${items.length}` });

/** Opens the dialog over `items` and returns each listed row's text once the servers agree with Cloud's list. */
async function listed(items: DeletionItem[]) {
  const load = vi.fn().mockResolvedValue(check(items));
  render(<DeletionDialog open onOpenChange={vi.fn()} title="Delete shop?" place="shop" confirmLabel="Delete" items={items}
    callbacks={{ load, confirm: vi.fn() }} />);
  await waitFor(() => expect(load).toHaveBeenCalled());
  await waitFor(() => expect(screen.getByRole("button", { name: "Delete" })).toBeTruthy());
  return screen.getAllByRole("listitem").map((item) => item.textContent ?? "");
}

describe("the deletion list", () => {
  it("names up to four things, data first, with their sizes", async () => {
    const rows = await listed([service("web"), volume("pg-data", 12.4), branch("fix-api"), volume("cache", 300 / 1024)]);
    expect(rows.map((row) => row.replace(/\s*[\d.]+ [KMG]?B$/, ""))).toEqual(["pg-data", "cache", "web", "fix-api"]);
    expect(rows[0]).toContain("12.4 GB");
    expect(rows[1]).toContain("300 MB");
  });

  it("past four, names the two biggest volumes and counts the rest by kind", async () => {
    const rows = await listed([
      ...Array.from({ length: 71 }, (_, index) => service(`svc-${index}`)),
      volume("media", 4.8), volume("backups", 22.1), volume("logs", 0.1), volume("tmp", 0.2),
      ...Array.from({ length: 26 }, (_, index) => branch(`pr-${index}`)),
    ]);
    expect(rows).toHaveLength(5);
    expect(rows[0]).toContain("backups");
    expect(rows[1]).toContain("media");
    expect(rows[2]).toContain("2 more volumes");
    expect(rows[2]).toMatch(/30\d(\.\d)? MB/);
    expect(rows[3]).toContain("71 services");
    expect(rows[4]).toContain("26 branches");
  });

  it("totals a group's size only when every member has one", async () => {
    const rows = await listed([service("a"), service("b"), volume("x", 3), volume("y", 2), volume("w", 1), volume("z")]);
    const group = rows.find((row) => row.includes("2 more volumes"));
    expect(group).toBeDefined();
    expect(group).not.toMatch(/\d B|[KMG]B/);
  });
});

describe("DeletionDialog", () => {
  function renderDialog(callbacks: DeletionCallbacks<string>, onOpenChange = vi.fn()) {
    render(
      <DeletionDialog open onOpenChange={onOpenChange} title="Delete staging?" place="shop/staging" confirmLabel="Delete"
        items={[service("web")]} callbacks={callbacks} />,
    );
    return onOpenChange;
  }

  it("confirms once the servers answered and the place is typed", async () => {
    const confirm = vi.fn().mockResolvedValue(undefined);
    const onOpenChange = renderDialog({ load: vi.fn().mockResolvedValue(check([service("web"), volume("uploads", 1.2)])), confirm });
    expect(screen.getByText("web")).toBeTruthy();
    await screen.findByText("uploads");

    const button = screen.getByRole("button", { name: "Delete" });
    expect(button.hasAttribute("disabled")).toBe(true);
    fireEvent.change(screen.getByRole("textbox", { name: "Type shop/staging to confirm" }), { target: { value: "shop/staging" } });
    fireEvent.click(button);

    await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false));
    expect(confirm).toHaveBeenCalledWith("evidence:2");
  });

  it("says what it is doing while the deletion runs", async () => {
    let finish = () => {};
    const confirm = vi.fn(() => new Promise<void>((resolve) => { finish = resolve; }));
    render(<DeletionDialog open onOpenChange={vi.fn()} title="Remove web-1?" place="web-1" confirmLabel="Remove" pendingLabel="Removing"
      callbacks={{ load: vi.fn().mockResolvedValue(check([])), confirm }} />);
    const input = await screen.findByRole("textbox", { name: "Type web-1 to confirm" });
    fireEvent.change(input, { target: { value: "web-1" } });
    await waitFor(() => expect(screen.getByRole("button", { name: "Remove" }).hasAttribute("disabled")).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Remove" }));

    await screen.findByText("Removing");
    expect(input.closest("form")?.getAttribute("aria-busy")).toBe("true");
    finish();
  });

  it("names disclosure controls by the group they show and hide", async () => {
    const items = [service("web"), service("api"), service("worker"), branch("preview-one"), branch("preview-two")];
    render(<DeletionDialog open onOpenChange={vi.fn()} title="Delete shop?" place="shop" confirmLabel="Delete" items={items}
      callbacks={{ load: vi.fn().mockResolvedValue(check(items)), confirm: vi.fn() }} />);
    const services = await screen.findByRole("button", { name: "Show 3 services" });
    expect(screen.getByRole("button", { name: "Show 2 branches" })).toBeTruthy();
    fireEvent.click(services);
    expect(screen.getByText("api")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Hide 3 services" }).getAttribute("aria-expanded")).toBe("true");
  });

  it("marks what the servers add to Cloud's list as new, first", async () => {
    renderDialog({ load: vi.fn().mockResolvedValue(check([service("web"), volume("leftover", 3)])), confirm: vi.fn() });
    await screen.findByText("New");
    expect(screen.getAllByRole("listitem")[0]?.textContent).toContain("leftover");
  });

  it("marks what the servers add to an empty Cloud list, but nothing in a list only the servers know", async () => {
    const load = vi.fn().mockResolvedValue(check([volume("leftover", 3)]));
    render(<DeletionDialog open onOpenChange={vi.fn()} title="Delete empty?" place="shop/empty" confirmLabel="Delete" items={[]}
      callbacks={{ load, confirm: vi.fn() }} />);
    await screen.findByText("New");
    cleanup();

    render(<DeletionDialog open onOpenChange={vi.fn()} title="Remove hel-1?" place="hel-1" confirmLabel="Remove"
      callbacks={{ load, confirm: vi.fn() }} />);
    await screen.findByText("leftover");
    expect(screen.queryByText("New")).toBeNull();
  });

  it("ends no sentence on a colon while there is nothing to list yet", () => {
    render(<DeletionDialog open onOpenChange={vi.fn()} title="Delete empty?" place="empty" confirmLabel="Delete" items={[]}
      callbacks={{ load: () => new Promise<ReturnType<typeof check>>(() => {}), confirm: vi.fn() }} />);
    expect(screen.getByRole("alertdialog").textContent).toContain("You're deleting empty.");
  });

  it("says what failed and retries the servers", async () => {
    const load = vi.fn()
      .mockRejectedValueOnce(new Error("Can't reach your servers. Check they're online, then try again."))
      .mockResolvedValue(check([service("web")]));
    renderDialog({ load, confirm: vi.fn() });

    expect((await screen.findByRole("alert")).textContent).toContain("Can't reach your servers");
    fireEvent.change(screen.getByRole("textbox", { name: /to confirm/ }), { target: { value: "shop/staging" } });
    expect(screen.getByRole("button", { name: "Delete" }).hasAttribute("disabled")).toBe(true);

    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Delete" }).hasAttribute("disabled")).toBe(false));
    expect(load).toHaveBeenCalledTimes(2);
  });

  it("keeps Delete for another try when the deletion fails, and says why", async () => {
    const confirm = vi.fn().mockRejectedValueOnce(new TypeError("Failed to fetch")).mockResolvedValue(undefined);
    const load = vi.fn().mockResolvedValue(check([service("web")]));
    const onOpenChange = renderDialog({ load, confirm });
    fireEvent.change(screen.getByRole("textbox", { name: /to confirm/ }), { target: { value: "shop/staging" } });
    await waitFor(() => expect(screen.getByRole("button", { name: "Delete" }).hasAttribute("disabled")).toBe(false));

    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    expect((await screen.findByRole("alert")).textContent).toBe("Couldn't reach Ployz Cloud. Check your connection and try again.");
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
    await waitFor(() => expect(screen.getByRole("button", { name: "Delete" }).hasAttribute("disabled")).toBe(false));

    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false));
    expect(confirm).toHaveBeenCalledTimes(2);
    expect(load).toHaveBeenCalledTimes(1);
  });

  it("marks what appeared since it opened and asks again, typed afresh, with the new evidence", async () => {
    const appeared = volume("uploads-old", 0.4);
    const confirm = vi.fn().mockResolvedValueOnce(check([service("web"), appeared])).mockResolvedValue(undefined);
    const onOpenChange = renderDialog({ load: vi.fn().mockResolvedValue(check([service("web")])), confirm });
    await waitFor(() => expect(screen.getByRole("button", { name: "Delete" })).toBeTruthy());
    fireEvent.change(screen.getByRole("textbox", { name: /to confirm/ }), { target: { value: "shop/staging" } });
    await waitFor(() => expect(screen.getByRole("button", { name: "Delete" }).hasAttribute("disabled")).toBe(false));

    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    await screen.findByText("New");
    expect(screen.getByText("uploads-old")).toBeTruthy();
    expect(onOpenChange).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Delete" }).hasAttribute("disabled")).toBe(true);

    fireEvent.change(screen.getByRole("textbox", { name: /to confirm/ }), { target: { value: "shop/staging" } });
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false));
    expect(confirm).toHaveBeenLastCalledWith("evidence:2");
  });
});
