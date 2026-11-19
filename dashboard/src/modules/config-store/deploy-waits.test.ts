import { expect, it } from "vitest";
import { deployWaits } from "./deploy-waits";

const servers = [{ id: "m1", name: "machine-1", region: "fsn" }, { id: "m2", name: "machine-2", region: "syd" }];
const volumes = [{ id: "v1", name: "pg-data", mounts: [{ service: "postgres" }] }];
const copy = (machineId: string, role: "writer" | "slot" = "writer") => ({ machineId, name: `shop-production_vol-v1`, role });
const postgres = { name: "postgres", replicas: 1 };
const web = { name: "web", replicas: 2 };

it("names a Service whose Volume is on a Server outside the region it is kept in, in the Deploy's words", () => {
  const moved = deployWaits({ services: [web, postgres], home: "syd", volumes, copies: [copy("m1")], servers });
  expect(moved).toEqual([{ service: "postgres", why: "pg-data is on machine-1" }]);
  const pinned = deployWaits({ services: [{ ...postgres, replicas: { syd: 1 } }], home: "fsn", volumes, copies: [copy("m1")], servers });
  expect(pinned).toEqual([{ service: "postgres", why: "pg-data is on machine-1" }]);
});

it("names no Service whose Volume has a copy in its region, or that runs in any region", () => {
  expect(deployWaits({ services: [postgres], home: "fsn", volumes, copies: [copy("m1")], servers })).toEqual([]);
  expect(deployWaits({ services: [postgres], home: null, volumes, copies: [copy("m1")], servers })).toEqual([]);
  expect(deployWaits({ services: [postgres], home: "syd", volumes, copies: [copy("m1"), copy("m2", "slot")], servers })).toEqual([]);
  // Not deployed yet: no Server holds it.
  expect(deployWaits({ services: [postgres], home: "syd", volumes, copies: [], servers })).toEqual([]);
});

it("names each Service kept in a region no Server is in, once the Servers are known", () => {
  const waits = deployWaits({ services: [web, { name: "api", replicas: { fsn: 1, fra: 1, nbg: 1 } }], home: "fra", volumes: [], copies: [], servers });
  expect(waits).toEqual([{ service: "web", why: "no Server in fra" }, { service: "api", why: "no Server in fra or nbg" }]);
  expect(deployWaits({ services: [web], home: "fra", volumes: [], copies: [], servers: [] })).toEqual([]);
});
