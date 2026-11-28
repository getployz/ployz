import { expect, test } from "vitest";
import { firstTunnelState } from "./servers-page-state";

const servers = (doors: ReadonlyArray<readonly ("public" | "tunnel")[]>, partial = false) => ({
  servers: doors.map((held, index) => ({ id: `m${index}`, name: `web-${index}`, doors: [...held] })),
  hostnames: [],
  partial,
});

test("a Cloudflare choice the tab holds reads as its own step until it ends", () => {
  const base = { foundedClosed: false, setup: null, observed: null };
  expect(firstTunnelState({ ...base, held: { kind: "waiting", token: "t", zone: "zenploy.dev" } })).toEqual({ kind: "waiting", zone: "zenploy.dev" });
  expect(firstTunnelState({ ...base, held: { kind: "setting-up", token: "t", zone: "zenploy.dev" } })).toEqual({ kind: "setting-up", zone: "zenploy.dev" });
  expect(firstTunnelState({ ...base, held: { kind: "failed", token: "t", zone: "zenploy.dev", message: "No." } }))
    .toEqual({ kind: "failed", zone: "zenploy.dev", message: "No." });
  expect(firstTunnelState({ ...base, held: { kind: "done", zone: "zenploy.dev", switched: true } })).toEqual({ kind: "done", zone: "zenploy.dev", switched: true });
});

test("a Cluster founded closed that no tab is setting up is unfinished only while its Servers are seen holding no Door", () => {
  const closed = { held: undefined, foundedClosed: true, setup: null };
  expect(firstTunnelState({ ...closed, observed: servers([[]]) })).toEqual({ kind: "unfinished" });
  expect(firstTunnelState({ ...closed, observed: null })).toEqual({ kind: "none" });
  expect(firstTunnelState({ ...closed, observed: servers([[]], true) })).toEqual({ kind: "none" });
  expect(firstTunnelState({ ...closed, observed: servers([["public"]]) })).toEqual({ kind: "none" });
  expect(firstTunnelState({ ...closed, observed: servers([["tunnel"]]) })).toEqual({ kind: "none" });
  expect(firstTunnelState({ held: undefined, foundedClosed: false, setup: null, observed: servers([[]]) })).toEqual({ kind: "none" });
  expect(firstTunnelState({ ...closed, setup: { zone: "zenploy.dev", tunnelName: "ployz-ada", tunnelId: null, state: "setting_up" }, observed: servers([[]]) }))
    .toEqual({ kind: "none" });
});
