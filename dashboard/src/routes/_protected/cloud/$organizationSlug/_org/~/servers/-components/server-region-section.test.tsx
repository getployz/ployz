// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import * as policyFunctions from "#/modules/machines/server-policy.functions";
import { ServerRegionSection } from "./server-region-section";

let requestChange: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  requestChange = vi.spyOn(policyFunctions, "requestServerPolicyChangeServerFn").mockResolvedValue(undefined);
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
const fleet = [fsn, machine("2", "syd"), machine("3", "fsn"), bare];

async function pick(name: string) {
  fireEvent.click(screen.getByRole("combobox", { name: "Region" }));
  const option = await screen.findByRole("option", { name });
  fireEvent.pointerEnter(option, { pointerType: "mouse" });
  fireEvent.pointerMove(option, { pointerType: "mouse" });
  fireEvent.mouseMove(option);
  fireEvent.click(option);
}

function section(shown: RuntimeMachineRecord, allowed = true, machines = fleet) {
  render(
    <ServerRegionSection
      machine={shown}
      machines={machines}
      organizationSlug="acme"
      gate={allowed ? { allowed: true, reason: undefined } : { allowed: false, reason: "Only an owner or admin can change how a server runs." }}
    />,
  );
}

it("shows a pick of the regions the fleet holds, each once, then a way to clear or add one", async () => {
  section(bare);
  fireEvent.click(screen.getByRole("combobox", { name: "Region" }));
  const options = (await screen.findAllByRole("option")).map((option) => option.textContent);
  expect(options).toEqual(["fsn", "syd", "No region", "New region…"]);
});

it("offers the lowercase spelling when Servers are labelled with two spellings of one region", async () => {
  section(bare, true, [machine("5", "FSN"), machine("6", "fsn"), bare]);
  fireEvent.click(screen.getByRole("combobox", { name: "Region" }));
  const options = (await screen.findAllByRole("option")).map((option) => option.textContent);
  expect(options).toEqual(["fsn", "No region", "New region…"]);
});

it("shows a region named like a menu entry as that region, and does not clear it", () => {
  section(machine("5", "_none"), true, [machine("5", "_none")]);
  expect(screen.getByRole("combobox", { name: "Region" }).textContent).toContain("_none");
  expect(requestChange).not.toHaveBeenCalled();
});

it("sends the picked region as a change and shows it queued until the Server reports it", async () => {
  section(bare);
  await pick("syd");
  expect(requestChange).toHaveBeenCalledWith({
    data: { organizationSlug: "acme", machineId: "4".repeat(32), change: { region: "syd" } },
  });
  expect(screen.getByText("Queued")).toBeTruthy();
  expect(screen.getByRole("combobox", { name: "Region" }).textContent).toContain("syd");
});

it("clears a region with a null", async () => {
  section(fsn);
  await pick("No region");
  expect(requestChange).toHaveBeenCalledWith({
    data: { organizationSlug: "acme", machineId: "1".repeat(32), change: { region: null } },
  });
});

it("types a new region, refuses a bad one inline without sending, and sends a good one", async () => {
  section(bare);
  await pick("New region…");
  const input = await screen.findByRole("textbox", { name: "New region" });

  fireEvent.change(input, { target: { value: "new york" } });
  expect(screen.getByText("Use up to 63 lowercase letters, digits and hyphens. Start with a letter or digit.")).toBeTruthy();
  fireEvent.keyDown(input, { key: "Enter" });
  fireEvent.change(input, { target: { value: "-lhr" } });
  fireEvent.keyDown(input, { key: "Enter" });
  expect(requestChange).not.toHaveBeenCalled();

  fireEvent.change(input, { target: { value: "LHR" } });
  expect((input as HTMLInputElement).value).toBe("lhr");
  fireEvent.keyDown(input, { key: "Enter" });
  expect(requestChange).toHaveBeenCalledWith({
    data: { organizationSlug: "acme", machineId: "4".repeat(32), change: { region: "lhr" } },
  });
});

it("leaves the field read-only for a member who can't change policy", () => {
  section(fsn, false);
  const trigger = screen.getByRole("combobox", { name: "Region" });
  expect(trigger.hasAttribute("disabled") || trigger.getAttribute("aria-disabled") === "true" || trigger.hasAttribute("data-disabled")).toBe(true);
  expect(trigger.getAttribute("title")).toBe("Only an owner or admin can change how a server runs.");
});
