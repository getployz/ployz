// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { RowId, ServiceListing, SyncRow, SyncView } from "@ployz/sdk";
import { DhkemX25519HkdfSha256 } from "@hpke/dhkem-x25519";
import { Suspense } from "react";
import { toast } from "sonner";
import { afterEach, assert, beforeEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as functions from "#/modules/config-store/store.functions";
import * as sealingFunctions from "#/modules/sealing/sealing.functions";
import type { StoreWriteResult } from "#/modules/config-store/store.contract";
import { storeViewPrefix, syncQuery } from "#/modules/config-store/store-view.queries";
import { asTestDouble } from "#/lib/test-double";
import { SyncDialog } from "./SyncDialog";

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.spyOn(sealingFunctions, "readSealingFn").mockResolvedValue({ organizationId: "org", sealing: { state: "no_server" } });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); document.body.replaceChildren(); });

const fixApi = { project: "shop", environment: "fix-api" };
const summary = (name: string) => ({ id: `id-${name}`, project: "shop", name, revision: 1 });
const id = (value: string) => value as RowId;
const row = (row: string, node: string, name: string | null, extra: Partial<SyncRow> = {}): SyncRow => ({
  row: id(row), node, kind: "service", name, change: "changed", from: null, into: null, ticked: true, requires: null, secret: null, held_by: null, ...extra,
});
const secret = {};
const rows = [
  row("a:variables.LOG_LEVEL", "api", "env.LOG_LEVEL", { from: "debug", into: "warn", change: "conflict" }),
  // Changed in production too, but a secret: its value never syncs, so Secret is what it says.
  row("a:variables.TOKEN", "api", "env.TOKEN", { from: { secret: true }, change: "conflict", secret }),
  row("w:node", "worker", null, { change: "new" }),
  row("w:variables.MODE", "worker", "env.MODE", { from: "fast", change: "new", requires: id("w:node") }),
  row("w:variables.KEY", "worker", "env.KEY", { from: { secret: true }, change: "new", secret, requires: id("w:node") }),
];
const syncView = (extra: Partial<SyncView> = {}): SyncView => ({
  from: summary("fix-api"), into: summary("production"), version: "4:abc", rows, proposal: null,
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
        <SyncDialog organizationSlug="acme" from={fixApi} into="production" closable={() => closable} onClose={() => {}} onSynced={() => {}} />
      </Suspense>
    </QueryClientProvider>,
  );
  return { write, queryClient };
}

const PRODUCTION_ID = "0e7c2f3a-5b1d-4c8e-9f60-1a2b3c4d5e6f";
const API_ID = "7d4e9a12-3c5b-4f6e-8a90-b1c2d3e4f5a6";

/** A Cluster key in a ready sealing read, returned so a test can tell two keys apart. */
async function sealingReady() {
  const kem = new DhkemX25519HkdfSha256();
  const raw = new Uint8Array(await kem.serializePublicKey((await kem.generateKeyPair()).publicKey));
  const publicKey = btoa(String.fromCharCode(...raw));
  vi.spyOn(sealingFunctions, "readSealingFn").mockResolvedValue({ organizationId: "org", sealing: { state: "ready", publicKey } });
  return publicKey;
}

/** production holds api, under a different id than in fix-api; worker is new to it. */
function receives() {
  const services = [asTestDouble<ServiceListing>()({ row: "a:node", id: API_ID, name: "api", change: null })];
  vi.mocked(functions.readStoreViewServerFn).mockImplementation(async ({ data }) => (data.query.query === "services"
    ? { ok: true, value: { view: "services", environment: { ...summary("production"), id: PRODUCTION_ID }, services } }
    : { ok: true, value: { view: "sync", ...syncView() } }) as never);
}

const dialog = async () => within(await screen.findByRole("dialog", { name: "Make production match fix-api" }));
const section = (sync: Awaited<ReturnType<typeof dialog>>, name: string) =>
  within(sync.getByRole("region", { name })).getAllByRole("listitem").map((item) => item.textContent);

it("shows each change with at most one badge, Sealed ahead of Changed, and a value field only on a secret", async () => {
  open();
  const sync = await dialog();
  expect(sync.getByText("These become production's changes to deploy.")).toBeTruthy();
  expect(section(sync, "api")).toEqual(["LOG_LEVELChanged in productionwarndebug", "TOKENSealed"]);
  expect(section(sync, "worker")).toEqual(["ServiceNew", "MODENewfast", "KEYSealed"]);
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

it("moves a row to never synced at once, and back, while the Store answers", async () => {
  const { write } = open();
  write.mockReturnValue(new Promise(() => {}));
  const sync = await dialog();
  fireEvent.click(sync.getByRole("checkbox", { name: /^MODE/u }));
  fireEvent.click(sync.getByRole("button", { name: "Never sync" }));
  await waitFor(() => expect(section(sync, "worker")).toEqual(["ServiceNew", "KEYSealed"]));
  fireEvent.click(sync.getByRole("button", { name: "2 never synced" }));
  const list = within(sync.getByRole("list", { name: "Never synced" }));
  expect(list.getByText("MODE")).toBeTruthy();

  const [stripe] = list.getAllByRole("button", { name: "Sync again" });
  fireEvent.click(stripe ?? assert.fail("no Sync again"));
  await waitFor(() => expect(sync.getByRole("button", { name: "1 never synced" })).toBeTruthy());
  expect(list.queryByText("STRIPE_KEY")).toBeNull();
});

it("syncs a PR Environment into production now, with production's value for a secret and no Close", async () => {
  await sealingReady();
  const { write } = open({ closable: false, view: syncView({ rows: [row("a:variables.TOKEN", "api", "env.TOKEN", { secret: {} })] }) });
  receives();
  const sync = await dialog();
  expect(sync.getByText("These become production's changes to deploy.")).toBeTruthy();
  expect(sync.queryByRole("checkbox", { name: /Close fix-api/u })).toBeNull();
  fireEvent.change(sync.getByLabelText("Set production's value of TOKEN"), { target: { value: "s3cret" } });
  fireEvent.click(sync.getByRole("button", { name: "Sync 1 change" }));
  await vi.waitFor(() => expect(write).toHaveBeenCalledTimes(1));
  expect(JSON.stringify(write.mock.calls)).not.toContain("s3cret");
  expect(write.mock.calls[0]?.[0].data.command).toMatchObject({
    command: "sync", into: { environment: "production" }, close_after: false,
    values: { "a:variables.TOKEN": { sealedFor: PRODUCTION_ID, value: { scope: { service: API_ID }, key: "TOKEN" } } },
  });
});

it("holds a row another source's included change holds: unticked, Included with that source, no Never sync", async () => {
  open({ view: syncView({ rows: [
    row("a:variables.LOG_LEVEL", "api", "env.LOG_LEVEL", { from: "debug", into: "warn", change: "conflict", ticked: false, held_by: "staging" }),
    row("a:variables.MODE", "api", "env.MODE", { from: "fast" }),
  ] }) });
  const sync = await dialog();
  expect(section(sync, "api")[0]).toContain("Included with staging");
  const held = sync.getByRole("checkbox", { name: /^LOG_LEVEL/u });
  expect(held.getAttribute("aria-checked")).toBe("false");
  expect(held.hasAttribute("data-disabled")).toBe(true);
  expect(sync.queryByRole("button", { name: "Never sync" })).toBeNull();
  expect(sync.getByRole("button", { name: "Sync 1 change" })).toBeTruthy();
});

it("sends one Sync id for a retry of the same picks, and a new one once the picks change", async () => {
  const { write } = open();
  const stale = { ok: false, refusal: { code: "conflict", message: "stale" } } as never;
  write.mockResolvedValueOnce(stale).mockResolvedValueOnce(stale);
  const sync = await dialog();
  fireEvent.click(sync.getByRole("button", { name: "Sync 5 changes" }));
  await sync.findByText("These changed since you opened them. Here they are now.");
  fireEvent.click(sync.getByRole("button", { name: "Sync 5 changes" }));
  await vi.waitFor(() => expect(write).toHaveBeenCalledTimes(2));
  await vi.waitFor(() => expect(sync.getByRole("button", { name: "Sync 5 changes" }).hasAttribute("disabled")).toBe(false));
  fireEvent.click(sync.getByRole("checkbox", { name: /^MODE/u }));
  fireEvent.click(sync.getByRole("button", { name: "Sync 4 changes" }));
  await vi.waitFor(() => expect(write).toHaveBeenCalledTimes(3));
  const ids = write.mock.calls.map(([{ data }]) => (data.command as { id?: string }).id);
  expect(ids[0]).toMatch(/^[0-9a-f-]{36}$/u);
  expect(ids[1]).toBe(ids[0]);
  expect(ids[2]).toMatch(/^[0-9a-f-]{36}$/u);
  expect(ids[2]).not.toBe(ids[0]);
});

it("reads Syncing and takes no second click while the Store answers, then offers the Sync again once it refuses", async () => {
  const { write } = open();
  let refuse!: (result: StoreWriteResult) => void;
  write.mockReturnValueOnce(new Promise<StoreWriteResult>((resolve) => { refuse = resolve; }));
  const sync = await dialog();
  fireEvent.click(sync.getByRole("button", { name: "Sync 5 changes" }));
  const syncing = await sync.findByRole("button", { name: /Syncing…/u });
  expect(syncing.hasAttribute("disabled")).toBe(true);
  fireEvent.click(syncing);
  expect(write).toHaveBeenCalledTimes(1);

  refuse({ ok: false, refusal: { code: "invalid", message: "No", details: {} } });
  await vi.waitFor(() => expect(sync.getByRole("button", { name: "Sync 5 changes" }).hasAttribute("disabled")).toBe(false));
  expect(sync.queryByRole("button", { name: /Syncing…/u })).toBeNull();
});

it("drops a flip once its change moves under the review, and says so", async () => {
  const { queryClient } = open();
  const sync = await dialog();
  fireEvent.click(sync.getByRole("checkbox", { name: /^LOG_LEVEL/u }));
  fireEvent.click(sync.getByRole("checkbox", { name: /^MODE/u }));
  expect(sync.getByRole("button", { name: "Sync 3 changes" })).toBeTruthy();
  const moved = rows.map((one) => one.row === id("a:variables.LOG_LEVEL") ? { ...one, from: "trace" } : one);
  act(() => queryClient.setQueryData([...storeViewPrefix("acme"), "session", "user", syncQuery(fixApi, "production")],
    { ok: true, value: { view: "sync", ...syncView({ rows: moved }) } }));
  await sync.findByText("These changed since you opened them. Here they are now.");
  expect(sync.getByRole("checkbox", { name: /^LOG_LEVEL/u }).getAttribute("aria-checked")).toBe("true");
  expect(sync.getByRole("checkbox", { name: /^MODE/u }).getAttribute("aria-checked")).toBe("false");
  expect(sync.getByRole("button", { name: "Sync 4 changes" })).toBeTruthy();
});

it("seals a typed value for the Service it lands in, afresh after a refusal and the same after a lost response, and holds a new Service's value", async () => {
  await sealingReady();
  const { write } = open();
  receives();
  write.mockResolvedValueOnce({ ok: false, refusal: { code: "conflict", message: "stale" } } as never);
  write.mockRejectedValueOnce(new TypeError("Failed to fetch"));
  const sync = await dialog();
  await sync.findByText("Set this value after syncing.");
  expect(sync.queryByLabelText("Set production's value of KEY")).toBeNull();
  fireEvent.change(sync.getByLabelText("Set production's value of TOKEN"), { target: { value: "token-value" } });
  fireEvent.click(sync.getByRole("button", { name: "Sync 5 changes" }));
  await sync.findByText("These changed since you opened them. Here they are now.");
  fireEvent.click(sync.getByRole("button", { name: "Sync 5 changes" }));
  await vi.waitFor(() => expect(write).toHaveBeenCalledTimes(2));
  await vi.waitFor(() => expect(sync.getByRole("button", { name: "Sync 5 changes" }).hasAttribute("disabled")).toBe(false));
  fireEvent.click(sync.getByRole("button", { name: "Sync 5 changes" }));
  await vi.waitFor(() => expect(write).toHaveBeenCalledTimes(3));

  const [first, afterRefusal, afterLoss] = write.mock.calls.map(([call]) => call.data.command);
  expect(JSON.stringify(write.mock.calls)).not.toContain("token-value");
  const values = first?.command === "sync" && first.values ? first.values : assert.fail("not a Sync");
  expect(Object.keys(values)).toEqual(["a:variables.TOKEN"]);
  expect(values["a:variables.TOKEN"]).toMatchObject({ sealedFor: PRODUCTION_ID, value: { scope: { service: API_ID }, key: "TOKEN" } });
  expect(afterRefusal).toMatchObject({ ...first, values: { "a:variables.TOKEN": { sealedFor: PRODUCTION_ID, value: { scope: { service: API_ID }, key: "TOKEN" } } } });
  expect(afterRefusal).not.toEqual(first);
  expect(afterLoss).toEqual(afterRefusal);
});

it("reads sealing afresh on a retry after a lost response, and seals again under a new key, under the same Sync", async () => {
  const before = await sealingReady();
  const { write } = open();
  receives();
  write.mockRejectedValueOnce(new TypeError("Failed to fetch"));
  const sync = await dialog();
  fireEvent.change(sync.getByLabelText("Set production's value of TOKEN"), { target: { value: "token-value" } });
  fireEvent.click(sync.getByRole("button", { name: "Sync 5 changes" }));
  await vi.waitFor(() => expect(write).toHaveBeenCalledTimes(1));
  await vi.waitFor(() => expect(sync.getByRole("button", { name: "Sync 5 changes" }).hasAttribute("disabled")).toBe(false));
  const after = await sealingReady();
  expect(after).not.toBe(before);
  fireEvent.click(sync.getByRole("button", { name: "Sync 5 changes" }));
  await vi.waitFor(() => expect(write).toHaveBeenCalledTimes(2));

  const [lost, retry] = write.mock.calls.map(([call]) => call.data.command);
  expect(JSON.stringify(write.mock.calls)).not.toContain("token-value");
  expect(retry).toMatchObject({ id: lost?.command === "sync" ? lost.id : assert.fail("not a Sync"),
    values: { "a:variables.TOKEN": { sealedFor: PRODUCTION_ID, value: { scope: { service: API_ID }, key: "TOKEN" } } } });
  expect(retry).not.toEqual(lost);
});
