// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  DeletionDialog, deletionItemKey, deletionRows, formatBytes, type DeletionCallbacks, type DeletionItem,
} from "./deletion-dialog";

afterEach(() => {
  cleanup();
  document.body.replaceChildren();
  document.body.style.removeProperty("overflow");
});

const GB = 1024 ** 3;
const service = (name: string): DeletionItem => ({ kind: "service", name });
const volume = (name: string, gb?: number): DeletionItem => ({ kind: "volume", name, bytes: gb === undefined ? undefined : gb * GB });
const branch = (name: string): DeletionItem => ({ kind: "branch", name });

describe("deletionRows", () => {
  it("names up to four things, data first", () => {
    const rows = deletionRows([service("web"), volume("pg-data", 12), branch("fix-api")], new Set());
    expect(rows.map((row) => row.type === "item" && row.item.name)).toEqual(["pg-data", "web", "fix-api"]);
  });

  it("past four, names the two biggest volumes and counts the rest by kind", () => {
    const items = [
      ...Array.from({ length: 71 }, (_, index) => service(`svc-${index}`)),
      volume("media", 4.8), volume("backups", 22.1), volume("logs", 0.1), volume("tmp", 0.2),
      ...Array.from({ length: 26 }, (_, index) => branch(`pr-${index}`)),
    ];
    const rows = deletionRows(items, new Set());
    expect(rows.map((row) => row.type === "item" ? row.item.name : row.label))
      .toEqual(["backups", "media", "2 more volumes", "71 services", "26 branches"]);
    const rest = rows[2];
    expect(rest?.type === "group" && rest.bytes).toBeCloseTo(0.3 * GB);
  });

  it("puts anything new first, where it can't scroll away", () => {
    const fresh = volume("uploads-old", 0.4);
    const rows = deletionRows([service("a"), service("b"), service("c"), service("d"), fresh], new Set([deletionItemKey(fresh)]));
    expect(rows[0]).toEqual({ type: "item", item: fresh, isNew: true });
  });

  it("totals a group's size only when every member has one", () => {
    const rows = deletionRows([service("a"), service("b"), volume("x", 3), volume("y", 2), volume("z")], new Set());
    const group = rows.find((row) => row.type === "group" && row.kind === "volume");
    expect(group?.type === "group" && group.bytes).toBeUndefined();
  });
});

describe("formatBytes", () => {
  it("reads like a dashboard", () => {
    expect(formatBytes(12.4 * GB)).toBe("12.4 GB");
    expect(formatBytes(300 * 1024 ** 2)).toBe("300 MB");
    expect(formatBytes(512)).toBe("512 B");
  });
});

/** A servers' check whose evidence names how many things it saw. */
const check = (items: DeletionItem[]) => ({ items, evidence: `evidence:${items.length}` });

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
    fireEvent.change(screen.getByRole("textbox", { name: /to confirm/ }), { target: { value: "shop/staging" } });
    fireEvent.click(button);

    await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false));
    expect(confirm).toHaveBeenCalledWith("evidence:2");
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

  it("says what failed and retries the servers", async () => {
    const load = vi.fn()
      .mockRejectedValueOnce(new Error("Can't reach your servers. Check they're online, then try again."))
      .mockResolvedValue(check([service("web")]));
    renderDialog({ load, confirm: vi.fn() });

    await screen.findByText(/Can't reach your servers/);
    fireEvent.change(screen.getByRole("textbox", { name: /to confirm/ }), { target: { value: "shop/staging" } });
    expect(screen.getByRole("button", { name: "Delete" }).hasAttribute("disabled")).toBe(true);

    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Delete" }).hasAttribute("disabled")).toBe(false));
    expect(load).toHaveBeenCalledTimes(2);
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
