// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { toast } from "sonner";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as storeFunctions from "#/modules/config-store/store.functions";
import type { StoredReport } from "#/modules/machines/waiting/checkin-wire";
import type { WaitingServerView } from "#/modules/machines/waiting/waiting-servers.server";
import * as waitingFunctions from "#/modules/machines/waiting/waiting.functions";
import { EnrollSheet } from "./enroll-sheet";

let start: ReturnType<typeof vi.spyOn>;
let write: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  vi.stubGlobal("matchMedia", (query: string) => ({ matches: false, media: query, addEventListener() {}, removeEventListener() {} }));
  vi.spyOn(waitingFunctions, "planServerEnrollmentServerFn").mockImplementation(async ({ data }) => ({
    rows: data.ids.map((id) => ({ waitingServerId: id, fleetTokenId: "fleet", hostname: id, role: "joiner" as const,
      instruction: { name: id, storage: { kind: "zfs" as const }, policy: { labels: {}, acceptsBuilds: true, acceptsServices: true, acceptsIngress: true } } })),
    skipped: [], spread: { providers: 1, networks: 1 }, serversOffline: false,
  }));
  start = vi.spyOn(waitingFunctions, "startServerEnrollmentServerFn").mockImplementation(async ({ data }) => ({
    attempts: data.ids.map((id) => ({ id: crypto.randomUUID(), waitingServerId: id, role: "joiner" as const, name: id, state: "instructed" as const })),
    skipped: [], spread: { providers: 1, networks: 1 },
  }));
  write = vi.spyOn(storeFunctions, "writeStoreServerFn").mockResolvedValue({ ok: true, value: { written: "region_named", key: "sydney", name: "Sydney" } } as never);
  vi.spyOn(storeFunctions, "readStoreViewServerFn").mockImplementation(async ({ data }) => {
    const { query } = data;
    if (query.query === "regions") return { ok: true, value: { view: "regions", regions: [{ key: "default", name: "Default" }, { key: "fsn", name: "Falkenstein" }] } } as never;
    if (query.query === "new_region" && query.name.toLowerCase() === "fsn") {
      return { ok: false, refusal: { code: "conflict", message: "Region Falkenstein (fsn) already exists.", details: null } } as never;
    }
    return { ok: true, value: { view: "new_region", key: "sydney" } } as never;
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const report: StoredReport = {
  hostname: "web", os: { id: "ubuntu", version: "24.04" }, kernel: "6.8.0", arch: "x86_64", virt: "kvm", cpuCount: 4,
  memoryTotalBytes: 8 * 1024 ** 3, disks: [], docker: { kind: "absent" }, cgroup: "v2",
  zfs: { kind: "eligible", targets: [{ kind: "main_disk" }] }, blockers: [], warnings: [],
};

function host(hostname: string): WaitingServerView {
  return {
    id: `id-${hostname}`, report: { ...report, hostname }, observedIp: "203.0.113.7", fleetToken: "brisk-otter", status: "waiting",
    firstSeenAt: "2026-10-17T09:59:00Z", lastCheckInAt: "2026-10-17T09:59:55Z", arrivalAt: "2026-10-17T09:59:00Z", returned: null,
  };
}

function sheet() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  render(
    <QueryClientProvider client={queryClient}>
      <EnrollSheet organizationSlug="ada" servers={[host("syd-1"), host("syd-2")]} regionLabels={["default", "fsn"]} hasFirst onClose={vi.fn()} />
    </QueryClientProvider>,
  );
}

const regionFor = (hostname: string) => screen.getByRole<HTMLSelectElement>("combobox", { name: `${hostname} · 203.0.113.7` });
const enroll = () => screen.getAllByRole<HTMLButtonElement>("button", { name: "Enroll 2" }).at(-1) as HTMLButtonElement;

it("starts each Server in Default, offers the regions by name, and sends each one's region, naming a new one once enrolled", async () => {
  sheet();
  await waitFor(() => expect(regionFor("syd-1").value).toBe("default"));
  expect([...regionFor("syd-1").options].map((option) => option.textContent)).toEqual(["Default", "Falkenstein", "New region…"]);

  fireEvent.change(regionFor("syd-1"), { target: { value: "new region" } });
  fireEvent.change(screen.getByRole("textbox", { name: "New region for syd-1" }), { target: { value: "Sydney" } });
  fireEvent.change(regionFor("syd-2"), { target: { value: "fsn" } });
  await waitFor(() => expect(enroll().disabled).toBe(false));
  fireEvent.click(enroll());

  await waitFor(() => expect(start).toHaveBeenCalledWith({
    data: expect.objectContaining({ ids: ["id-syd-1", "id-syd-2"], regions: { "id-syd-1": "sydney", "id-syd-2": "fsn" } }),
  }));
  await waitFor(() => expect(write).toHaveBeenCalledWith(expect.objectContaining({
    data: expect.objectContaining({ command: { command: "name_region", key: "sydney", name: "Sydney", labels: expect.any(Array), creates: true } }),
  })));
});

it("refuses a new region whose key a region has in any case, naming it, and enrolls nothing", async () => {
  sheet();
  await waitFor(() => expect(regionFor("syd-1").value).toBe("default"));
  fireEvent.change(regionFor("syd-1"), { target: { value: "new region" } });
  fireEvent.change(screen.getByRole("textbox", { name: "New region for syd-1" }), { target: { value: "FSN" } });
  expect(await screen.findByText("Region Falkenstein (fsn) already exists.")).toBeTruthy();
  expect(enroll().disabled).toBe(true);
  expect(start).not.toHaveBeenCalled();
  expect(write).not.toHaveBeenCalled();
});

it("refuses two different names that make one region's key, and enrolls nothing", async () => {
  sheet();
  await waitFor(() => expect(regionFor("syd-1").value).toBe("default"));
  for (const [host, typed] of [["syd-1", "Sydney"], ["syd-2", "SYDNEY"]] as const) {
    fireEvent.change(regionFor(host), { target: { value: "new region" } });
    fireEvent.change(screen.getByRole("textbox", { name: `New region for ${host}` }), { target: { value: typed } });
  }
  expect((await screen.findAllByText("Sydney and SYDNEY make the same region. Use one name.")).length).toBeGreaterThan(0);
  expect(enroll().disabled).toBe(true);
  expect(start).not.toHaveBeenCalled();
});

it("says when a new region's name wasn't saved after the Servers were enrolled", async () => {
  const error = vi.spyOn(toast, "error").mockReturnValue("t");
  write.mockResolvedValue({ ok: false, refusal: { code: "invalid_argument", message: "A region named Sydney has another key.", details: null } });
  sheet();
  await waitFor(() => expect(regionFor("syd-1").value).toBe("default"));
  fireEvent.change(regionFor("syd-1"), { target: { value: "new region" } });
  fireEvent.change(screen.getByRole("textbox", { name: "New region for syd-1" }), { target: { value: "Sydney" } });
  await waitFor(() => expect(enroll().disabled).toBe(false));
  fireEvent.click(enroll());
  await waitFor(() => expect(error).toHaveBeenCalledWith("Sydney is set, but its name wasn’t saved.", expect.objectContaining({
    description: expect.stringContaining("shows as sydney until you rename it from Servers"),
  })));
});
