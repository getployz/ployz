// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import type { RowId, SyncRow, SyncView } from "@ployz/sdk";
import { Suspense } from "react";
import { toast } from "sonner";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as functions from "#/modules/config-store/store.functions";
import { storeViewPrefix, syncQuery } from "#/modules/config-store/store-view.queries";
import { SyncDialog } from "./SyncDialog";

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); document.body.replaceChildren(); });

const fixApi = { project: "shop", environment: "fix-api" };
const summary = (name: string) => ({ id: `id-${name}`, project: "shop", name, revision: 1 });
const id = (value: string) => value as RowId;
const row = (row: string, node: string, name: string | null, extra: Partial<SyncRow> = {}): SyncRow => ({
  row: id(row), node, kind: "service", name, change: "changed", from: null, into: null, ticked: true, requires: null, secret: null, ...extra,
});
const secret = { needs_value: true, held: false };
const rows = [
  row("a:variables.LOG_LEVEL", "api", "env.LOG_LEVEL", { from: "debug", into: "warn", change: "conflict" }),
  // Changed in production too, but a secret: its value never syncs, so Secret is what it says.
  row("a:variables.TOKEN", "api", "env.TOKEN", { from: { secret: true }, change: "conflict", secret }),
  row("w:node", "worker", null, { change: "new" }),
  row("w:variables.MODE", "worker", "env.MODE", { from: "fast", change: "new", requires: id("w:node") }),
  row("w:variables.KEY", "worker", "env.KEY", { from: { secret: true }, change: "new", secret, requires: id("w:node") }),
];
const syncView = (extra: Partial<SyncView> = {}): SyncView => ({
  from: summary("fix-api"), into: summary("production"), at_merge: null, version: "4:abc", rows,
  never_synced: [{ row: id("a:variables.STRIPE_KEY"), node: "api", kind: "service", name: "env.STRIPE_KEY", marks: [{ environment: "fix-api", row: id("a:variables.STRIPE_KEY") }] }],
  ...extra,
});

function open({ view = syncView(), closable = true } = {}) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  vi.spyOn(toast, "error").mockImplementation(() => "toast");
  // Always into the explicit receiver: nothing else is cached.
  queryClient.setQueryData([...storeViewPrefix("acme"), "session", "user", syncQuery(fixApi, "production")], { ok: true, value: { view: "sync", ...view } });
  vi.spyOn(functions, "readStoreViewServerFn").mockResolvedValue({ ok: true, value: { view: "sync", ...view } } as never);
  const write = vi.spyOn(functions, "writeStoreServerFn").mockResolvedValue({ ok: true, value: { written: "synced" } } as never);
  render(
    <QueryClientProvider client={queryClient}>
      <Suspense fallback={null}>
        <SyncDialog organizationSlug="acme" from={fixApi} into="production" closable={closable} onClose={() => {}} onSynced={() => {}} />
      </Suspense>
    </QueryClientProvider>,
  );
  return { write };
}

const dialog = async () => within(await screen.findByRole("dialog", { name: "Sync to production" }));
const section = (sync: Awaited<ReturnType<typeof dialog>>, name: string) =>
  within(sync.getByRole("region", { name })).getAllByRole("listitem").map((item) => item.textContent);

it("shows each change with at most one badge, Secret ahead of Changed, and a value field only on a secret", async () => {
  open();
  const sync = await dialog();
  expect(sync.getByText("These changes from fix-api become production's changes to deploy.")).toBeTruthy();
  expect(section(sync, "api")).toEqual(["LOG_LEVELChanged in productionwarndebug", "TOKENSecret"]);
  expect(section(sync, "worker")).toEqual(["ServiceNew", "MODENewfast", "KEYSecret"]);
  expect(sync.getAllByLabelText(/^Set production's value of/u).map((field) => field.getAttribute("aria-label"))).toEqual([
    "Set production's value of TOKEN", "Set production's value of KEY",
  ]);
  expect(sync.getByRole("button", { name: "Sync 5 changes" })).toBeTruthy();
});

it("lets a new Service's variable be left out on its own; leaving the Service out leaves its settings out", async () => {
  open();
  const sync = await dialog();
  fireEvent.click(sync.getByRole("checkbox", { name: /^MODE/u }));
  expect(sync.getByRole("button", { name: "Sync 4 changes" })).toBeTruthy();
  // Left out on its own, it can be marked Never sync.
  expect(sync.getByRole("button", { name: "Never sync" })).toBeTruthy();
  fireEvent.click(sync.getByRole("checkbox", { name: /^Service/u }));
  expect(sync.getByRole("button", { name: "Sync 2 changes" })).toBeTruthy();
  expect(sync.getByRole("checkbox", { name: /^KEY/u }).hasAttribute("data-disabled")).toBe(true);
  expect(sync.queryByRole("button", { name: "Never sync" })).toBeNull();
});

it("lists what is never synced from the footer, and offers to close the Branch after", async () => {
  open();
  const sync = await dialog();
  expect(sync.getByRole("checkbox", { name: "Close fix-api after syncing" }).getAttribute("aria-checked")).toBe("true");
  fireEvent.click(sync.getByRole("button", { name: "1 never synced" }));
  const list = within(sync.getByRole("list", { name: "Never synced" }));
  expect(list.getByText("STRIPE_KEY")).toBeTruthy();
  expect(list.getByText(/api, marked in fix-api/u)).toBeTruthy();
});

it("says a Conditional Sync goes live at the merge, offers no Close, and shows a value already held", async () => {
  open({ view: syncView({ at_merge: 142, rows: [row("a:variables.TOKEN", "api", "env.TOKEN", { secret: { needs_value: true, held: true } })] }) });
  const sync = await dialog();
  expect(sync.getByText("These changes from fix-api go live in production when #142 merges.")).toBeTruthy();
  expect(sync.queryByRole("checkbox", { name: /Close fix-api/u })).toBeNull();
  expect(sync.getByLabelText("Set production's value of TOKEN").getAttribute("placeholder")).toBe("Value held");
});
