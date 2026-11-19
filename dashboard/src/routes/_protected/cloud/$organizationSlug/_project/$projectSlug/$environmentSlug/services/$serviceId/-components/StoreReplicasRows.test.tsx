// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import type { JsonValue } from "@ployz/sdk";
import { useState } from "react";
import { renderToString } from "react-dom/server";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { replicaRows } from "#/modules/config-store/replica-rows";
import type { SharedRegion } from "#/modules/config-store/deploy-waits";
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

type Held = { capped?: string; attached?: boolean; deployed?: JsonValue; sharing?: SharedRegion };

/** The rows over a value that takes each write, as the Store hands it back to them. */
function staged(start: JsonValue, home: string | null, held: Held = {}) {
  const onChange = vi.fn((_: JsonValue) => Promise.resolve());
  const elsewhere = { set: (_: JsonValue) => {} };
  function Staged() {
    const [value, setValue] = useState(start);
    elsewhere.set = setValue;
    const kept = replicaRows(value, home).flatMap(({ region }) => region ?? []);
    return (
      <StoreReplicasRows options={regionOptions(servers, [], kept)} value={value} deployed={held.deployed ?? start} home={home}
        capped={held.capped} oneRegion={held.attached ?? (held.capped !== undefined || held.sharing !== undefined)}
        sharing={held.sharing ?? null} changed={false}
        onChange={(next) => { setValue(next); return onChange(next); }} />
    );
  }
  return { Staged, onChange, setElsewhere: (next: JsonValue) => act(() => elsewhere.set(next)) };
}

function rows(start: JsonValue, home: string | null, held: Held = {}) {
  const { Staged, onChange } = staged(start, home, held);
  render(<Staged />);
  return onChange;
}

const regionOf = (row: number) => screen.getByRole("combobox", { name: `Region of row ${row}` });
const shownIn = (row: number) => regionOf(row).querySelector("[data-slot=select-value]")?.textContent;
const addRegion = () => screen.queryByRole("button", { name: "Add region" });
const more = (label: string) => fireEvent.click(within(screen.getByRole("group", { name: label })).getByRole("button", { name: "One more" }));

async function pick(row: number, name: string, listed?: (names: (string | null)[]) => void) {
  fireEvent.click(regionOf(row));
  const option = await screen.findByRole("option", { name });
  listed?.(screen.getAllByRole("option").map(({ textContent }) => textContent));
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

it("holds a Service to the region the Services it shares a Volume with run in, and says why", () => {
  const sharing = { region: "fsn", sharers: ["postgres"], volume: "pg-data", located: true };
  rows(1, "fsn", { sharing });
  expect(regionOf(1).hasAttribute("disabled")).toBe(true);
  expect(shownIn(1)).toBe("fsn");
  expect(document.body.textContent).toContain("Must run in fsn. It shares the Volume pg-data with postgres, and pg-data is in fsn.");
  expect(addRegion()).toBeNull();
});

it("offers only the sharers' region to a Service set apart from them", async () => {
  const onChange = rows({ syd: 1 }, "fsn", { sharing: { region: "fsn", sharers: ["postgres"], volume: "pg-data", located: true } });
  await pick(1, "fsn · server-1", (names) => expect(names).toEqual(["fsn · server-1"]));
  expect(onChange).toHaveBeenLastCalledWith({ fsn: 1 });
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

const click = (name: string) => fireEvent.click(screen.getByRole("button", { name }));

it("stages nothing when the region just added is removed again", () => {
  const onChange = rows(2, "fsn");
  click("Add region");
  expect(onChange).toHaveBeenLastCalledWith({ fsn: 2, syd: 1 });
  click("Remove syd");
  expect(onChange).toHaveBeenLastCalledWith(2);
});

it("keeps a region named with ployz set through a count change and a region added and removed", () => {
  const onChange = rows({ fsn: 2 }, "fsn", { deployed: 1 });
  more("Replicas in fsn");
  click("Add region");
  click("Remove syd");
  expect(onChange).toHaveBeenLastCalledWith({ fsn: 3 });
});

it("keeps a region named with ployz set while the page is open through a count change and a region added and removed", () => {
  const { Staged, onChange, setElsewhere } = staged(2, "fsn");
  render(<Staged />);
  setElsewhere({ fsn: 2 });
  more("Replicas in fsn");
  expect(onChange).toHaveBeenLastCalledWith({ fsn: 3 });
  click("Add region");
  click("Remove syd");
  expect(onChange).toHaveBeenLastCalledWith({ fsn: 3 });
});

it("keeps a pin set elsewhere again after a bare count in between, through a count change", () => {
  const { Staged, onChange, setElsewhere } = staged({ fsn: 1 }, "fsn", { deployed: 1 });
  render(<Staged />);
  more("Replicas in fsn");
  expect(onChange).toHaveBeenLastCalledWith({ fsn: 2 });
  setElsewhere(4);
  setElsewhere({ fsn: 2 });
  more("Replicas in fsn");
  expect(onChange).toHaveBeenLastCalledWith({ fsn: 3 });
});

it("names the region after One more then Add region in quick succession, and is bare again once back where it was deployed", () => {
  const onChange = rows(1, "fsn");
  more("Replicas in fsn");
  click("Add region");
  expect(onChange.mock.calls.map(([value]) => value)).toEqual([2, { fsn: 2, syd: 1 }]);
  click("Remove syd");
  click("One fewer");
  expect(onChange).toHaveBeenLastCalledWith(1);
});

it("names the region left when it opens on two and can't know the Service ever had a bare count", () => {
  const onChange = rows({ fsn: 3, syd: 1 }, "fsn", { deployed: 1 });
  click("Remove syd");
  expect(onChange).toHaveBeenLastCalledWith({ fsn: 3 });
});

it("adds a region a Server is in with one replica", () => {
  const onChange = rows(2, "fsn");
  click("Add region");
  expect(onChange).toHaveBeenLastCalledWith({ fsn: 2, syd: 1 });
});

it("keeps every control disabled until the page hydrates, then takes One more and Add region", () => {
  const { Staged, onChange } = staged({ fsn: 1, syd: 1 }, "fsn");
  const container = document.body.appendChild(document.createElement("div"));
  container.innerHTML = renderToString(<Staged />);
  const usable = [...within(container).getAllByRole("button"), ...within(container).getAllByRole("combobox")]
    .filter((control) => !control.hasAttribute("disabled")).map((control) => control.getAttribute("aria-label"));
  expect(usable).toEqual([]);
  render(<Staged />, { container, hydrate: true });
  click("Remove syd");
  more("Replicas in fsn");
  click("Add region");
  expect(onChange.mock.calls.map(([value]) => value)).toEqual([{ fsn: 1 }, { fsn: 2 }, { fsn: 2, syd: 1 }]);
});
