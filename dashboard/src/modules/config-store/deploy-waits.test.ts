import { expect, it } from "vitest";
import { deployWaits, mountSplit, sharedRegion } from "./deploy-waits";

const servers = [{ id: "m1", name: "machine-1", region: "fsn" }, { id: "m2", name: "machine-2", region: "syd" }];
const volumes = [{ id: "v1", name: "pg-data", mounts: [{ service: "postgres" }] }];
const copy = (machineId: string, role: "writer" | "slot" = "writer") => ({ machineId, name: `shop-production_vol-v1`, role });
const postgres = { name: "postgres", replicas: 1 };
const web = { name: "web", replicas: 2 };

it("names a Service whose Volume is on a Server outside the region it is kept in, in the Deploy's words", () => {
  const moved = deployWaits({ services: [web, postgres], home: "syd", volumes, copies: [copy("m1")], servers });
  expect(moved).toEqual([{ service: "postgres", why: "its Volume pg-data is in fsn and can't move to syd yet. Run it in fsn instead", refused: false }]);
  const pinned = deployWaits({ services: [{ ...postgres, replicas: { syd: 1 } }], home: "fsn", volumes, copies: [copy("m1")], servers });
  expect(pinned).toEqual([{ service: "postgres", why: "its Volume pg-data is in fsn and can't move to syd yet. Run it in fsn instead", refused: false }]);
  const unlabelled = deployWaits({ services: [postgres], home: "syd", volumes, copies: [copy("m1")], servers: [{ id: "m1", name: "machine-1", region: null }, { id: "m2", name: "machine-2", region: "syd" }] });
  expect(unlabelled).toEqual([{ service: "postgres", why: "its Volume pg-data is on machine-1, a Server in no region, and can't move to syd yet. Give machine-1 the region syd instead", refused: false }]);
});

it("names a Service the Deploy refuses, in the Engine's words, when a Service kept elsewhere shares its Volume", () => {
  const reader = { name: "reader", replicas: { syd: 1 } };
  const shared = [{ id: "v1", name: "pg-data", mounts: [{ service: "postgres" }, { service: "reader" }] }];
  const head = (service: string, other: string) => `${service} and ${other} share the Volume pg-data, so they have to run in the same region. `;
  const located = deployWaits({ services: [postgres, reader], home: "fsn", volumes: shared, copies: [copy("m1")], servers });
  expect(located).toEqual([{ service: "reader", why: `${head("reader", "postgres")}pg-data is in fsn and reader is set to syd. Set reader to fsn`, refused: true }]);
  const split = deployWaits({ services: [{ ...postgres, replicas: { fsn: 1 } }, { name: "reader", replicas: 1 }], home: "syd", volumes: shared, copies: [copy("m1")], servers });
  expect(split).toEqual(located);
  expect(deployWaits({ services: [postgres, { name: "reader", replicas: 1 }], home: "syd", volumes: shared, copies: [copy("m1")], servers }).every(({ refused }) => !refused)).toBe(true);
});

it("gives every Service sharing a Volume on a Server with no region the one way out", () => {
  const unlabelled = deployWaits({ services: [{ ...postgres, replicas: { fsn: 1 } }, { name: "reader", replicas: { syd: 1 } }], home: "fsn",
    volumes: [{ id: "v1", name: "pg-data", mounts: [{ service: "postgres" }, { service: "reader" }] }], copies: [copy("m1")],
    servers: [{ id: "m1", name: "machine-1", region: null }, { id: "m2", name: "machine-2", region: "syd" }] });
  const way = "pg-data is on machine-1, which has no region. Give machine-1 a region, then set them all to it";
  expect(unlabelled.map(({ service, why }) => [service, why.split(". ").slice(1).join(". ")])).toEqual([["postgres", way], ["reader", way]]);
});

it("asks Services sharing a Volume not made yet to agree on a region, not to swap", () => {
  const unmade = deployWaits({ services: [postgres, { name: "reader", replicas: { syd: 1 } }], home: "fsn",
    volumes: [{ id: "v1", name: "pg-data", mounts: [{ service: "postgres" }, { service: "reader" }] }], copies: [], servers });
  expect(unmade.map(({ why }) => why.endsWith("Set them all to one region"))).toEqual([true, true]);
});

it("holds a Service in Scale to the region its Volume is in, even when a sharer is set elsewhere", () => {
  const shared = [{ id: "v1", name: "pg-data", mounts: [{ service: "extra" }, { service: "postgres" }, { service: "reader" }] }];
  const services = [{ name: "extra", replicas: { syd: 1 } }, postgres, { name: "reader", replicas: 1 }];
  expect(sharedRegion("postgres", { services, home: "fsn", volumes: shared, copies: [copy("m1")], servers }))
    .toEqual({ region: "fsn", volume: "pg-data", sharers: ["extra", "reader"], made: true });
  expect(sharedRegion("postgres", { services, home: "fsn", volumes: shared, copies: [], servers })).toBeNull();
  expect(sharedRegion("extra", { services, home: "fsn", volumes: shared, copies: [], servers }))
    .toEqual({ region: "fsn", volume: "pg-data", sharers: ["postgres", "reader"], made: false });
  expect(sharedRegion("postgres", { services, home: "fsn", volumes: shared, copies: [copy("m1")], servers: [{ id: "m1", name: "machine-1", region: null }] })).toBeNull();
});

it("says before a mount that a Service set to another region can't share the Volume, in the Deploy's words", () => {
  const volume = { id: "v1", name: "pg-data", mounts: [{ service: "postgres" }] };
  const lens = { services: [postgres, { name: "extra", replicas: { syd: 1 } }, { name: "near", replicas: 1 }], home: "fsn", copies: [copy("m1")], servers };
  expect(mountSplit("extra", volume, lens)).toBe("extra and postgres share the Volume pg-data, so they have to run in the same region. pg-data is in fsn and extra is set to syd. Set extra to fsn");
  expect(mountSplit("near", volume, lens)).toBeNull();
});

it("names a Service whose Volume has only a mirror in its region, as the Deploy looks for the copy it writes to", () => {
  const waits = deployWaits({ services: [postgres], home: "syd", volumes, copies: [copy("m1"), copy("m2", "slot")], servers });
  expect(waits).toEqual([{ service: "postgres", why: "its Volume pg-data is in fsn and can't move to syd yet. Run it in fsn instead", refused: false }]);
});

it("names no Service whose Volume is written in its region, or that runs in any region", () => {
  expect(deployWaits({ services: [postgres], home: "fsn", volumes, copies: [copy("m1")], servers })).toEqual([]);
  expect(deployWaits({ services: [postgres], home: "fsn", volumes, copies: [copy("m1"), copy("m2", "slot")], servers })).toEqual([]);
  expect(deployWaits({ services: [postgres], home: null, volumes, copies: [copy("m1")], servers })).toEqual([]);
  // Not deployed yet: no Server holds it.
  expect(deployWaits({ services: [postgres], home: "syd", volumes, copies: [], servers })).toEqual([]);
});

it("names each Service kept in a region no Server is in, once the Servers are known", () => {
  const waits = deployWaits({ services: [web, { name: "api", replicas: { fsn: 1, fra: 1, nbg: 1 } }], home: "fra", volumes: [], copies: [], servers });
  expect(waits).toEqual([{ service: "web", why: "no Server in fra", refused: false }, { service: "api", why: "no Server in fra or nbg", refused: false }]);
  expect(deployWaits({ services: [web], home: "fra", volumes: [], copies: [], servers: [] })).toEqual([]);
});
