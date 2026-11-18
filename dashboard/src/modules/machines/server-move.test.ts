import { expect, it } from "vitest";
import { leavingServer, placedServices } from "./server-move";

const servers = [
  { id: "1", name: "np1-vm-1", region: "default", acceptsServices: true },
  { id: "2", name: "np1-vm-2", region: "default", acceptsServices: true },
  { id: "3", name: "fsn-1", region: "FSN", acceptsServices: true },
  { id: "4", name: "fsn-2", region: "fsn", acceptsServices: true },
  { id: "5", name: "fsn-builder", region: "fsn", acceptsServices: false },
];

const service = (name: string, region: string | null) =>
  ({ key: `shop-production/${name}`, name, environment: "shop/production", region });

const owned = [
  { namespace: "shop-production", project: "shop", environment: "production", region: "default" },
  { namespace: "blog-production", project: "blog", environment: "production", region: "fsn" },
  { namespace: "lab-production", project: "lab", environment: "production" },
];
const running = (identity: string) => {
  const slash = identity.indexOf("/");
  return { identity, name: identity.slice(slash + 1), namespace: identity.slice(0, slash) };
};

it("names each Service's Environment and the region that Environment runs in", () => {
  expect(placedServices([running("shop-production/web"), running("blog-production/api"), running("lab-production/cron")], owned)).toEqual([
    { key: "shop-production/web", name: "web", environment: "shop/production", region: "default" },
    { key: "blog-production/api", name: "api", environment: "blog/production", region: "fsn" },
    { key: "lab-production/cron", name: "cron", environment: "lab/production", region: null },
  ]);
});

it("gives a Service no Environment owns no region, so it never leaves", () => {
  const stray = placedServices([running("old-production/web"), { identity: "web", name: "web", namespace: null }], owned);
  expect(stray.map(({ region }) => region)).toEqual([null, null]);
  expect(leavingServer("2", "syd", stray, servers)).toEqual([]);
});

it("moves nothing off a Server while no Environment names a region", () => {
  expect(leavingServer("2", "syd", placedServices([running("lab-production/cron")], owned), servers)).toEqual([]);
});

it("lists what runs in another region than the Server's new one, with the Servers in that region that take Services", () => {
  const placed = [service("web", "default"), service("postgres", "DEFAULT"), service("api", "fsn"), service("cron", "syd"), service("docs", null)];
  expect(leavingServer("2", "syd", placed, servers)).toEqual([
    { ...service("web", "default"), to: ["np1-vm-1"] },
    { ...service("postgres", "DEFAULT"), to: ["np1-vm-1"] },
    { ...service("api", "fsn"), to: ["fsn-1", "fsn-2"] },
  ]);
});

it("lists everything with a region when the Server leaves every region, and says when no other Server is in one", () => {
  expect(leavingServer("2", null, [service("web", "default"), service("cron", "syd")], servers)).toEqual([
    { ...service("web", "default"), to: ["np1-vm-1"] },
    { ...service("cron", "syd"), to: [] },
  ]);
});
