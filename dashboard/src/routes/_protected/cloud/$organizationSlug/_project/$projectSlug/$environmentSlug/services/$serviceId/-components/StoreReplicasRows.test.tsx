// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import type { JsonValue } from "@ployz/sdk";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { replicaRows } from "#/modules/config-store/replica-rows";
import { regionOptions } from "#/modules/machines/server-regions";
import { StoreReplicasRows } from "./StoreReplicasRows";

let servers: { name: string; region: string }[] = [];

const two = [{ name: "server-1", region: "fsn" }, { name: "server-2", region: "syd" }];

beforeEach(() => {
  servers = two;
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  Element.prototype.scrollIntoView = () => {};
  Element.prototype.hasPointerCapture = () => false;
  Element.prototype.releasePointerCapture = () => {};
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function rows(value: JsonValue, home: string | null, held: { capped?: string; attached?: boolean; deployed?: JsonValue } = {}) {
  const onChange = vi.fn(() => Promise.resolve());
  const kept = replicaRows(value, home).flatMap(({ region }) => region ?? []);
  render(<StoreReplicasRows options={regionOptions(servers, [], kept)} value={value} deployed={held.deployed ?? value} home={home}
    capped={held.capped} oneRegion={held.attached ?? held.capped !== undefined} changed={false} onChange={onChange} />);
  return onChange;
}

const regionOf = (row: number) => screen.getByRole("combobox", { name: `Region of row ${row}` });
const shownIn = (row: number) => regionOf(row).querySelector("[data-slot=select-value]")?.textContent;
const addRegion = () => screen.queryByRole("button", { name: "Add region" });
const more = (label: string) => fireEvent.click(within(screen.getByRole("group", { name: label })).getByRole("button", { name: "One more" }));

async function pick(row: number, name: string) {
  fireEvent.click(regionOf(row));
  const option = await screen.findByRole("option", { name });
  fireEvent.pointerEnter(option, { pointerType: "mouse" });
  fireEvent.pointerMove(option, { pointerType: "mouse" });
  fireEvent.mouseMove(option);
  fireEvent.click(option);
}

it("starts the rows in the Environment's default region and offers another", () => {
  rows(2, "fsn");
  expect(shownIn(1)).toBe("fsn");
  expect(screen.getByRole("group", { name: "Replicas in fsn" }).textContent).toBe("2");
  expect(addRegion()).not.toBeNull();
});

it("reads Any region with no default region, whatever the Servers' regions, and offers no other", () => {
  servers = [];
  rows(2, null);
  expect(shownIn(1)).toBe("Any region");
  expect(addRegion()).toBeNull();
  cleanup();
  servers = two;
  rows({ fsn: 2 }, null);
  expect(shownIn(1)).toBe("fsn");
  expect(addRegion()).toBeNull();
});

it("never offers Add region while a Volume limits the count", () => {
  rows(1, "fsn", { capped: "Limited to 1 while pg-data is attached." });
  expect(screen.getByText(/Limited to 1 while/).textContent).toContain("pg-data");
  expect(addRegion()).toBeNull();
});

it("keeps a Service with a shared-write Volume in one region, with more replicas there", () => {
  const onChange = rows(2, "fsn", { attached: true });
  expect(addRegion()).toBeNull();
  more("Replicas in fsn");
  expect(onChange).toHaveBeenLastCalledWith(3);
});

it("keeps the region a Service names when only its count changes", () => {
  const onChange = rows({ fsn: 2 }, "fsn");
  more("Replicas in fsn");
  expect(onChange).toHaveBeenLastCalledWith({ fsn: 3 });
});

it("keeps both counts when a row moves to a region whose key is any", async () => {
  servers = [...two, { name: "server-3", region: "any" }];
  const onChange = rows({ fsn: 2, syd: 1 }, "fsn");
  await pick(1, "any · server-3");
  expect(onChange).toHaveBeenLastCalledWith({ any: 2, syd: 1 });
});

it("stages nothing when the region just added is removed again", () => {
  const onChange = rows({ fsn: 2, syd: 1 }, "fsn", { deployed: 2 });
  fireEvent.click(screen.getByRole("button", { name: "Remove syd" }));
  expect(onChange).toHaveBeenLastCalledWith(2);
});

it("adds a region a Server is in with one replica", () => {
  const onChange = rows(2, "fsn");
  fireEvent.click(screen.getByRole("button", { name: "Add region" }));
  expect(onChange).toHaveBeenLastCalledWith({ fsn: 2, syd: 1 });
});
