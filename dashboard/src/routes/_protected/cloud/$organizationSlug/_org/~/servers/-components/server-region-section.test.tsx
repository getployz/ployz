// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { NamedRegion } from "@ployz/sdk";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as storeFunctions from "#/modules/config-store/store.functions";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import type { OwnedNamespaces } from "#/modules/machines/server-regions.hooks";
import * as policyFunctions from "#/modules/machines/server-policy.functions";
import { ServerRegionSection } from "./server-region-section";

let requestChange: ReturnType<typeof vi.spyOn>;
let write: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  requestChange = vi.spyOn(policyFunctions, "requestServerPolicyChangeServerFn").mockResolvedValue(undefined);
  write = vi.spyOn(storeFunctions, "writeStoreServerFn").mockResolvedValue({ ok: true, value: { written: "region_named", key: "sydney-2", name: "Sydney 2" } } as never);
  vi.spyOn(storeFunctions, "readStoreViewServerFn").mockImplementation(async ({ data }) => (data.query.query === "new_region" && data.query.name === "FSN"
    ? { ok: false, refusal: { code: "conflict", message: "Region Falkenstein (fsn) already exists.", details: null } }
    : { ok: true, value: { view: "new_region", key: "sydney-2" } }) as never);
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  Element.prototype.scrollIntoView = () => {};
  Element.prototype.hasPointerCapture = () => false;
  Element.prototype.releasePointerCapture = () => {};
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function machine(id: string, region: string | null): RuntimeMachineRecord {
  return {
    id: id.repeat(32), name: `server-${id}`, subnet: null, publicIp: null, acceptsIngress: true, endpoints: [], acceptsBuilds: false, acceptsServices: true,
    storage: "pool", buildConcurrency: null, region, effectiveBuildConcurrency: 1, runningBuilds: 0, daemonVersion: "0.3.0-beta.2", capabilities: [],
    poolCommittableBytes: null, sealing: null, membership: "up", unanswered: false, observedContainerCount: 0, observedAt: "2026-10-14T00:00:00Z",
  };
}

const fsn = machine("1", "fsn");
const bare = machine("4", null);
const fleet = [fsn, machine("2", "syd"), machine("3", "default"), bare];
const REGIONS: NamedRegion[] = [{ key: "default", name: "Default" }, { key: "fsn", name: "Falkenstein" }, { key: "syd", name: "Sydney" }];

async function pick(name: string) {
  fireEvent.click(screen.getByRole("combobox", { name: "Region" }));
  const option = await screen.findByRole("option", { name });
  fireEvent.pointerEnter(option, { pointerType: "mouse" });
  fireEvent.pointerMove(option, { pointerType: "mouse" });
  fireEvent.mouseMove(option);
  fireEvent.click(option);
}

const NONE: OwnedNamespaces = { kind: "read", namespaces: [] };

function section(shown: RuntimeMachineRecord, { allowed = true, machines = fleet, regions = REGIONS, services = [], owned = NONE }: {
  allowed?: boolean;
  machines?: RuntimeMachineRecord[];
  regions?: NamedRegion[];
  services?: readonly { identity: string; name: string; namespace: string | null }[];
  owned?: OwnedNamespaces;
} = {}) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  render(
    <QueryClientProvider client={queryClient}>
      <ServerRegionSection
        machine={shown}
        machines={machines}
        services={services}
        owned={owned}
        regions={regions}
        organizationSlug="acme"
        gate={allowed ? { allowed: true, reason: undefined } : { allowed: false, reason: "Only an owner or admin can change how a server runs." }}
      />
    </QueryClientProvider>,
  );
}

it("shows the Server's region by its name, then every region by name, then a way to clear or add one", async () => {
  section(fsn);
  expect(screen.getByRole("combobox", { name: "Region" }).textContent).toContain("Falkenstein");
  fireEvent.click(screen.getByRole("combobox", { name: "Region" }));
  const options = (await screen.findAllByRole("option")).map((option) => option.textContent);
  expect(options).toEqual(["Default", "Falkenstein", "Sydney", "No region", "New region…"]);
});

it("shows a Server labelled in capitals as its region", () => {
  section(machine("5", "SYD"));
  expect(screen.getByRole("combobox", { name: "Region" }).textContent).toContain("Sydney");
});

it("shows a region named like a menu entry as that region, and does not clear it", () => {
  section(machine("5", "_none"), { machines: [machine("5", "_none")], regions: [] });
  expect(screen.getByRole("combobox", { name: "Region" }).textContent).toContain("_none");
  expect(requestChange).not.toHaveBeenCalled();
});

it("sends the picked region's key as a change and shows its name queued until the Server reports it", async () => {
  section(bare);
  await pick("Sydney");
  expect(requestChange).toHaveBeenCalledWith({
    data: { organizationSlug: "acme", machineId: "4".repeat(32), change: { region: "syd" } },
  });
  expect(screen.getByText("Queued")).toBeTruthy();
  expect(screen.getByRole("combobox", { name: "Region" }).textContent).toContain("Sydney");
});

it("clears a region with a null", async () => {
  section(fsn);
  await pick("No region");
  expect(requestChange).toHaveBeenCalledWith({
    data: { organizationSlug: "acme", machineId: "1".repeat(32), change: { region: null } },
  });
});

it("makes a new region from a typed name: the Server moves to its key and the region keeps the name", async () => {
  section(bare);
  await pick("New region…");
  const input = await screen.findByRole("textbox", { name: "New region" });
  fireEvent.change(input, { target: { value: "Sydney 2" } });
  fireEvent.keyDown(input, { key: "Enter" });
  await waitFor(() => expect(requestChange).toHaveBeenCalledWith({
    data: { organizationSlug: "acme", machineId: "4".repeat(32), change: { region: "sydney-2" } },
  }));
  expect(write).toHaveBeenCalledWith(expect.objectContaining({
    data: expect.objectContaining({ command: { command: "name_region", key: "sydney-2", name: "Sydney 2", labels: ["default", "fsn", "syd"], creates: true } }),
  }));
});

it("refuses a new region whose key a region has in any case, naming it, and moves nothing", async () => {
  section(bare);
  await pick("New region…");
  const input = await screen.findByRole("textbox", { name: "New region" });
  fireEvent.change(input, { target: { value: "FSN" } });
  fireEvent.keyDown(input, { key: "Enter" });
  expect(await screen.findByText("Region Falkenstein (fsn) already exists.")).toBeTruthy();
  expect(requestChange).not.toHaveBeenCalled();
  expect(write).not.toHaveBeenCalled();
});

it("leaves the field read-only for a member who can't change policy", () => {
  section(fsn, { allowed: false });
  const trigger = screen.getByRole("combobox", { name: "Region" });
  expect(trigger.hasAttribute("disabled") || trigger.getAttribute("aria-disabled") === "true" || trigger.hasAttribute("data-disabled")).toBe(true);
  expect(trigger.getAttribute("title")).toBe("Only an owner or admin can change how a server runs.");
});

const service = (namespace: string, name: string) => ({ identity: `${namespace}/${name}`, name, namespace });
const RUNNING = [service("shop-production", "web"), service("shop-production", "postgres"), service("blog-production", "api")];
const SHOP = { namespace: "shop-production", project: "shop", environment: "production", region: "default" };
const BLOG = { namespace: "blog-production", project: "blog", environment: "production", region: "fsn" };
const READ: OwnedNamespaces = { kind: "read", namespaces: [SHOP, BLOG] };
const vm3 = machine("3", "default");
const withDefaultPeer = [...fleet, machine("6", "default")];

it("asks before a move that sends Services off the Server, naming each with its Environment and where it goes", async () => {
  section(vm3, { services: RUNNING, owned: READ, machines: withDefaultPeer });
  await pick("Sydney");
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).getByRole("heading").textContent).toBe("Move server-3 to Sydney");
  expect(within(dialog).getByText("These leave server-3 on the next deploy.")).toBeTruthy();
  expect(within(dialog).getAllByRole("listitem").map((row) => row.textContent)).toEqual([
    "webshop/production→ server-6",
    "postgresshop/production→ server-6",
    "apiblog/production→ server-1",
  ]);
  expect(requestChange).not.toHaveBeenCalled();

  fireEvent.click(within(dialog).getByRole("button", { name: "Move server" }));
  await waitFor(() => expect(requestChange).toHaveBeenCalledWith({
    data: { organizationSlug: "acme", machineId: "3".repeat(32), change: { region: "syd" } },
  }));
});

it("keeps the Server where it was when the move is cancelled", async () => {
  section(vm3, { services: RUNNING, owned: READ, machines: withDefaultPeer });
  await pick("Sydney");
  fireEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(requestChange).not.toHaveBeenCalled();
  expect(screen.getByRole("combobox", { name: "Region" }).textContent).toContain("Default");
});

it("says so when no other Server is in a Service's region", async () => {
  section(vm3, { services: RUNNING.slice(0, 1), owned: READ });
  await pick("Sydney");
  expect(within(await screen.findByRole("dialog")).getAllByRole("listitem").map((row) => row.textContent)).toEqual([
    "webshop/productionNo server in Default",
  ]);
});

it("asks before a move even when the Service sets its own replica regions, which its Environment's region doesn't name", async () => {
  const replicated: OwnedNamespaces = { kind: "read", namespaces: [{ ...SHOP, region: "syd", replica_regions: { web: ["default"] } }] };
  section(vm3, { services: RUNNING.slice(0, 1), owned: replicated, machines: withDefaultPeer });
  await pick("Sydney");
  expect(within(await screen.findByRole("dialog")).getAllByRole("listitem").map((row) => row.textContent)).toEqual(["webshop/production→ server-6"]);
});

it("leaves out an offline Server as a place a Service can go", async () => {
  const down = { ...machine("6", "default"), membership: "down", unanswered: true } as const;
  section(vm3, { services: RUNNING.slice(0, 1), owned: READ, machines: [...fleet, down] });
  await pick("Sydney");
  expect(within(await screen.findByRole("dialog")).getAllByRole("listitem").map((row) => row.textContent)).toEqual([
    "webshop/productionNo server in Default",
  ]);
});

it("names the button after clearing a region, not moving the Server", async () => {
  section(vm3, { services: RUNNING, owned: READ, machines: withDefaultPeer });
  await pick("No region");
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).queryByRole("button", { name: "Move server" })).toBeNull();
  fireEvent.click(within(dialog).getByRole("button", { name: "Clear region" }));
  await waitFor(() => expect(requestChange).toHaveBeenCalledWith({
    data: { organizationSlug: "acme", machineId: "3".repeat(32), change: { region: null } },
  }));
});

it("holds the region while what runs here is being read, and says when it can't be", () => {
  section(vm3, { services: RUNNING, owned: { kind: "reading" } });
  expect(screen.getByText("Reading what runs here…")).toBeTruthy();
  expect(screen.getByRole("combobox", { name: "Region" }).hasAttribute("disabled")).toBe(true);
  cleanup();
  section(vm3, { services: RUNNING, owned: { kind: "failed" } });
  expect(screen.getByText("Couldn’t read what runs here. Reload to change the region.")).toBeTruthy();
  expect(screen.getByRole("combobox", { name: "Region" }).hasAttribute("disabled")).toBe(true);
});

it("asks before a move when an Environment's Working State couldn't be read, naming it instead of showing nothing", async () => {
  section(vm3, { services: RUNNING.slice(0, 1), owned: { kind: "read", namespaces: [{ ...SHOP, region: undefined, unread: true }] } });
  await pick("Sydney");
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).getByText(/Couldn’t read shop\/production/)).toBeTruthy();
  expect(requestChange).not.toHaveBeenCalled();
});
