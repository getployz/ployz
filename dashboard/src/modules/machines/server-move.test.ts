import { expect, it } from "vitest";
import { leavingServer, placedServices, unreadEnvironments } from "./server-move";

const server = (id: string, name: string, region: string, extra: { acceptsServices?: boolean; offline?: boolean } = {}) =>
  ({ id, name, region, acceptsServices: true, offline: false, ...extra });
const servers = [
  server("1", "np1-vm-1", "default"),
  server("2", "np1-vm-2", "default"),
  server("3", "fsn-1", "FSN"),
  server("4", "fsn-2", "fsn"),
  server("5", "fsn-builder", "fsn", { acceptsServices: false }),
];

const service = (name: string, ...regions: string[]) =>
  ({ key: `shop-production/${name}`, name, environment: "shop/production", regions, perRegion: false, unread: false });

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
    { key: "shop-production/web", name: "web", environment: "shop/production", regions: ["default"], perRegion: false, unread: false },
    { key: "blog-production/api", name: "api", environment: "blog/production", regions: ["fsn"], perRegion: false, unread: false },
    { key: "lab-production/cron", name: "cron", environment: "lab/production", regions: [], perRegion: false, unread: false },
  ]);
});

it("gives a Service no Environment owns no region, so it never leaves", () => {
  const stray = placedServices([running("old-production/web"), { identity: "web", name: "web", namespace: null }], owned);
  expect(stray.map(({ regions }) => regions)).toEqual([[], []]);
  expect(leavingServer("2", "syd", stray, servers)).toEqual([]);
});

it("moves nothing off a Server while no Environment names a region", () => {
  expect(leavingServer("2", "syd", placedServices([running("lab-production/cron")], owned), servers)).toEqual([]);
});

it("lists what runs in another region than the Server's new one, with the Servers in that region that take Services", () => {
  const placed = [service("web", "default"), service("postgres", "DEFAULT"), service("api", "fsn"), service("cron", "syd"), service("docs")];
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

it("takes a Service's own replica regions over its Environment's, which names none or another", () => {
  const sets: Parameters<typeof placedServices>[1] = [{
    namespace: "shop-production", project: "shop", environment: "production", region: "default",
    replica_regions: { web: ["fsn", "lhr"] },
  }, {
    namespace: "lab-production", project: "lab", environment: "production",
    replica_regions: { cron: ["fsn"] },
  }];
  const placed = placedServices([running("shop-production/web"), running("shop-production/db"), running("lab-production/cron")], sets);
  expect(placed.map(({ name, regions }) => [name, regions])).toEqual([["web", ["fsn", "lhr"]], ["db", ["default"]], ["cron", ["fsn"]]]);
  // Server 3 stays in FSN and 2 moves to syd: web and cron leave 2 for the FSN Servers, db for the other default one.
  expect(leavingServer("2", "syd", placed, servers).map(({ name, to }) => [name, to])).toEqual([
    ["web", ["fsn-1", "fsn-2"]],
    ["db", ["np1-vm-1"]],
    ["cron", ["fsn-1", "fsn-2"]],
  ]);
  // A Service in the region the Server joins stays, though its Environment names another.
  expect(leavingServer("2", "FSN", placed, servers).map(({ name }) => name)).toEqual(["db"]);
});

it("lists a Service that counts replicas in both the region a Server leaves and the one it joins", () => {
  const counted = placedServices(
    [running("shop-production/web")],
    [{ namespace: "shop-production", project: "shop", environment: "production", replica_regions: { web: ["default", "syd"] } }],
  );
  expect(counted.map(({ regions, perRegion }) => [regions, perRegion])).toEqual([[["default", "syd"], true]]);
  // Server 2 holds web's default replica; in syd it no longer does, though syd also has a count.
  expect(leavingServer("2", "syd", counted, servers).map(({ name, to }) => [name, to])).toEqual([["web", ["np1-vm-1"]]]);
  expect(leavingServer("2", "SYD", counted, servers).map(({ name }) => name)).toEqual(["web"]);
  // Staying where it is changes nothing, and a Server in neither counted region holds none of its replicas.
  expect(leavingServer("2", "default", counted, servers)).toEqual([]);
  expect(leavingServer("3", "syd", counted, servers).map(({ name }) => name)).toEqual([]);
  expect(leavingServer("3", "default", counted, servers)).toEqual([]);
});

it("leaves out an offline Server as a place a Service can go", () => {
  const down = [server("1", "np1-vm-1", "default", { offline: true }), server("2", "np1-vm-2", "default"), server("6", "np1-vm-3", "default")];
  expect(leavingServer("2", "syd", [service("web", "default")], down)).toEqual([{ ...service("web", "default"), to: ["np1-vm-3"] }]);
  expect(leavingServer("2", "syd", [service("web", "default")], down.slice(0, 2))).toEqual([{ ...service("web", "default"), to: [] }]);
});

it("names the Environments whose Working State couldn't be read, which leave nothing to compute", () => {
  const blog = { namespace: "blog-production", project: "blog", environment: "production", region: "fsn" };
  const placed = placedServices(
    [running("shop-production/web"), running("blog-production/api")],
    [{ namespace: "shop-production", project: "shop", environment: "production", unread: true }, blog],
  );
  expect(unreadEnvironments(placed)).toEqual(["shop/production"]);
  expect(unreadEnvironments(placedServices([running("blog-production/api")], owned))).toEqual([]);
});
