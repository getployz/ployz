import { expect, it } from "vitest";
import { deployWaits } from "./deploy-waits";

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
  const located = deployWaits({ services: [postgres, reader], home: "fsn", volumes: shared, copies: [copy("m1")], servers });
  expect(located).toEqual([{ service: "reader", why: "pg-data is in fsn, and reader runs in syd. Run reader in fsn instead", refused: true }]);
  const unlabelled = deployWaits({ services: [postgres, reader], home: "fsn", volumes: shared, copies: [copy("m1")], servers: [{ id: "m1", name: "machine-1", region: null }, { id: "m2", name: "machine-2", region: "syd" }] });
  const apart = "pg-data is shared by postgres in fsn and reader in syd. Run them in one region, or give each its own Volume";
  expect(unlabelled).toEqual([{ service: "postgres", why: apart, refused: true }, { service: "reader", why: apart, refused: true }]);
  expect(deployWaits({ services: [postgres, reader], home: "fsn", volumes: shared, copies: [], servers })).toEqual(unlabelled);
  expect(deployWaits({ services: [postgres, { name: "reader", replicas: 1 }], home: "syd", volumes: shared, copies: [copy("m1")], servers }).every(({ refused }) => !refused)).toBe(true);
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
