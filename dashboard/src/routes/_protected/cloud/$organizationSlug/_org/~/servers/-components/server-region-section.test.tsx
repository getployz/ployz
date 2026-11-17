// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { Region } from "@ployz/sdk";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as storeFunctions from "#/modules/config-store/store.functions";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import * as policyFunctions from "#/modules/machines/server-policy.functions";
import { ServerRegionSection } from "./server-region-section";

let requestChange: ReturnType<typeof vi.spyOn>;
let write: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  requestChange = vi.spyOn(policyFunctions, "requestServerPolicyChangeServerFn").mockResolvedValue(undefined);
  write = vi.spyOn(storeFunctions, "writeStoreServerFn").mockResolvedValue({ ok: true, value: { written: "region_named", key: "sydney-2", name: "Sydney 2" } } as never);
  // The Store's answers to the two names these tests type.
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
    id: id.repeat(32), name: `server-${id}`, publicIp: null, acceptsIngress: true, endpoints: [], acceptsBuilds: false, acceptsServices: true,
    storage: "pool", buildConcurrency: null, region, effectiveBuildConcurrency: 1, runningBuilds: 0, daemonVersion: "0.3.0-beta.2", capabilities: [],
    poolCommittableBytes: null, sealing: null, membership: "up", unanswered: false, observedContainerCount: 0, observedAt: "2026-10-14T00:00:00Z",
  };
}

const fsn = machine("1", "fsn");
const bare = machine("4", null);
const fleet = [fsn, machine("2", "syd"), machine("3", "default"), bare];
const REGIONS: Region[] = [{ key: "default", name: "Default" }, { key: "fsn", name: "Falkenstein" }, { key: "syd", name: "Sydney" }];

async function pick(name: string) {
  fireEvent.click(screen.getByRole("combobox", { name: "Region" }));
  const option = await screen.findByRole("option", { name });
  fireEvent.pointerEnter(option, { pointerType: "mouse" });
  fireEvent.pointerMove(option, { pointerType: "mouse" });
  fireEvent.mouseMove(option);
  fireEvent.click(option);
}

function section(shown: RuntimeMachineRecord, { allowed = true, machines = fleet, regions = REGIONS }: { allowed?: boolean; machines?: RuntimeMachineRecord[]; regions?: Region[] } = {}) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  render(
    <QueryClientProvider client={queryClient}>
      <ServerRegionSection
        machine={shown}
        machines={machines}
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
    data: expect.objectContaining({ command: { command: "name_region", key: "sydney-2", name: "Sydney 2" } }),
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
