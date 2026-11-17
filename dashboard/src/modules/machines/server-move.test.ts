import { expect, it } from "vitest";
import { leavingServer, placedServices } from "./server-move";

const servers = [
  { id: "1", name: "np1-vm-1", region: "default", acceptsServices: true },
  { id: "2", name: "np1-vm-2", region: "default", acceptsServices: true },
  { id: "3", name: "fsn-1", region: "FSN", acceptsServices: true },
  { id: "4", name: "fsn-2", region: "fsn", acceptsServices: true },
  { id: "5", name: "fsn-builder", region: "fsn", acceptsServices: false },
];

const service = (name: string, region: string | null, volume: string | null = null) =>
  ({ key: `shop-production/${name}`, name, volume, environment: "shop/production", region });

it("moves nothing off a Server while no Environment names a region", () => {
  const running = placedServices([{ identity: "shop-production/web", name: "web", namespace: "shop-production" }]);
  expect(leavingServer("2", "syd", running, servers)).toEqual([]);
});

it("lists what runs in another region than the Server's new one, with the Servers in that region that take Services", () => {
  const running = [service("web", "default"), service("postgres", "DEFAULT", "pg-data"), service("api", "fsn"), service("cron", "syd"), service("docs", null)];
  expect(leavingServer("2", "syd", running, servers)).toEqual([
    { ...service("web", "default"), to: ["np1-vm-1"] },
    { ...service("postgres", "DEFAULT", "pg-data"), to: ["np1-vm-1"] },
    { ...service("api", "fsn"), to: ["fsn-1", "fsn-2"] },
  ]);
});

it("lists everything with a region when the Server leaves every region, and says when no other Server is in one", () => {
  expect(leavingServer("2", null, [service("web", "default"), service("cron", "syd")], servers)).toEqual([
    { ...service("web", "default"), to: ["np1-vm-1"] },
    { ...service("cron", "syd"), to: [] },
  ]);
});
