import { expect, it } from "vitest";
import { firstServerRegion, regionLabels, regionOptions } from "./server-regions";

const server = (subnet: string | null, region: string | null, joinedAt: number | null = null) => ({ subnet, joinedAt, region });

it("starts a new Environment in the region of the Cluster's first Server, the lowest subnet, whatever its name", () => {
  expect(firstServerRegion([
    server("10.210.10.0/24", "default"),
    server("10.210.9.0/24", "FSN"),
    server("10.210.11.0/24", "default"),
  ])).toBe("fsn");
});

it("starts a new Environment where the Server that joined first is, though a newer one reused a lower subnet", () => {
  expect(firstServerRegion([server("10.210.0.0/24", "default", 2_000), server("10.210.1.0/24", "syd", 1_000)])).toBe("syd");
});

it("pins nothing in a mixed Cluster, whichever Server holds the lowest subnet", () => {
  expect(firstServerRegion([server("10.210.0.0/24", "default", 2_000), server("10.210.1.0/24", null)])).toBeNull();
});

it("pins nothing when the first Server has no region, though a later one does", () => {
  expect(firstServerRegion([server("10.210.0.0/24", null), server("10.210.1.0/24", "default")])).toBeNull();
});

it("pins nothing when a Server's subnet is not known yet, rather than guess the first", () => {
  expect(firstServerRegion([server(null, "default"), server("10.210.1.0/24", "fsn")])).toBeNull();
});

it("pins nothing for no Servers, or a first Server whose label is no region", () => {
  expect(firstServerRegion([])).toBeNull();
  expect(firstServerRegion([server("10.210.0.0/24", "eu/west"), server("10.210.1.0/24", "default")])).toBeNull();
});

it("lists the default region and each region the Servers are in, leaving out a label that is no region", () => {
  expect(regionLabels([{ region: "eu_west.1" }, { region: "eu/west" }, { region: null }, { region: "fsn" }])).toEqual(["default", "eu_west.1", "fsn"]);
});

it("offers every named region, one no Server is in too, after Any region", () => {
  const options = regionOptions([{ name: "server-2", region: "syd" }, { name: "server-1", region: "SYD" }], [{ key: "fsn", name: "Falkenstein" }, { key: "syd", name: "Sydney" }]);
  expect(options).toEqual([
    { key: null, name: "Any region", servers: [] },
    { key: "fsn", name: "Falkenstein", servers: [] },
    { key: "syd", name: "Sydney", servers: ["server-1", "server-2"] },
  ]);
});
