// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { RegionSelect } from "./RegionSelect";
import { regionOptions } from "./server-regions";

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  Element.prototype.scrollIntoView = () => {};
  Element.prototype.hasPointerCapture = () => false;
  Element.prototype.releasePointerCapture = () => {};
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const options = regionOptions(
  [{ name: "server-1", region: "fsn" }, { name: "server-2", region: "fsn" }, { name: "server-3", region: "any" }],
  [{ key: "fsn", name: "Falkenstein" }, { key: "syd", name: "Sydney" }],
);

async function pick(name: string) {
  fireEvent.click(screen.getByRole("combobox", { name: "Runs in" }));
  const option = await screen.findByRole("option", { name });
  fireEvent.pointerEnter(option, { pointerType: "mouse" });
  fireEvent.pointerMove(option, { pointerType: "mouse" });
  fireEvent.mouseMove(option);
  fireEvent.click(option);
}

it("picks a region whose key is any as that region, and Any region as none", async () => {
  const onChange = vi.fn();
  render(<RegionSelect label="Runs in" options={options} value="fsn" onChange={onChange} />);
  await pick("any · server-3");
  expect(onChange).toHaveBeenLastCalledWith("any");
  await pick("Any region");
  expect(onChange).toHaveBeenLastCalledWith(null);
});

it("lists each region by its name and Servers, and one with no Servers can't be picked", async () => {
  render(<RegionSelect label="Runs in" options={options} value={null} onChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("combobox", { name: "Runs in" }));
  expect(await screen.findByRole("option", { name: "Falkenstein · server-1, server-2" })).toBeTruthy();
  expect(screen.getByRole("option", { name: "Sydney · no Servers" }).getAttribute("aria-disabled")).toBe("true");
});
