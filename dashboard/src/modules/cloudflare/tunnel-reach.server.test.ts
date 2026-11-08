import { Effect } from "effect";
import { expect, it } from "vitest";
import { isPublicAddress, reachesCluster } from "./tunnel-reach.server";

it("takes only addresses a visitor could reach, never Cloud's own network", () => {
  for (const address of ["104.16.0.1", "2606:4700::1111", "172.32.0.1", "100.63.255.255"]) expect(isPublicAddress(address), address).toBe(true);
  for (const address of [
    "127.0.0.1", "10.1.2.3", "172.16.0.1", "172.31.255.255", "192.168.1.1", "169.254.169.254", "100.64.0.1", "0.0.0.0", "224.0.0.1", "255.255.255.255",
    "::1", "::", "fe80::1", "fd00::1", "::ffff:127.0.0.1", "::ffff:10.0.0.1", "not an address",
  ]) expect(isPublicAddress(address), address).toBe(false);
});

it("never fetches from an address typed in place of a hostname, or from a name that resolves inward", async () => {
  const machines = new Set(["machine-1"]);
  expect(await Effect.runPromise(reachesCluster("127.0.0.1", machines))).toBe(false);
  expect(await Effect.runPromise(reachesCluster("::1", machines))).toBe(false);
  expect(await Effect.runPromise(reachesCluster("localhost", machines))).toBe(false);
});
