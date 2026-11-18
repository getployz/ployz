import { expect, it } from "vitest";
import { firstServerRegion } from "./server-regions";

it("starts a new Environment in the region of the first Server that has one, by name", () => {
  expect(firstServerRegion([
    { name: "np1-vm-2", region: "default" },
    { name: "fsn-1", region: "FSN" },
    { name: "np1-vm-1", region: "default" },
  ])).toBe("fsn");
  expect(firstServerRegion([{ name: "a", region: null }, { name: "b", region: "Default" }])).toBe("default");
});

it("leaves a new Environment in any region while no Server has one", () => {
  expect(firstServerRegion([])).toBeNull();
  expect(firstServerRegion([{ name: "a", region: null }])).toBeNull();
});
