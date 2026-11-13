import { expect, test } from "vitest";
import { abandonCloudflare, firstTunnelState, foundingFirstServer, installGate, serversPageState } from "./servers-page-state";

test("zero servers with nothing waiting is first run without the intake tray", () => {
  expect(serversPageState({ list: "live", servers: 0, waiting: 0, enrolling: 0 })).toEqual({ kind: "first-run", intake: false });
});

test("a server arriving on first run opens the intake tray", () => {
  expect(serversPageState({ list: "live", servers: 0, waiting: 1, enrolling: 0 })).toEqual({ kind: "first-run", intake: true });
  expect(serversPageState({ list: "live", servers: 0, waiting: 0, enrolling: 1 })).toEqual({ kind: "first-run", intake: true });
});

test("servers make a fleet, and the tray shows only with something in it", () => {
  expect(serversPageState({ list: "live", servers: 3, waiting: 0, enrolling: 0 })).toEqual({ kind: "fleet", intake: false, stale: false });
  expect(serversPageState({ list: "stale", servers: 3, waiting: 2, enrolling: 0 })).toEqual({ kind: "fleet", intake: true, stale: true });
});

test("loading and unreachable keep the tray, which comes from Cloud", () => {
  expect(serversPageState({ list: "loading", servers: 0, waiting: 0, enrolling: 0 })).toEqual({ kind: "loading", intake: false });
  expect(serversPageState({ list: "unreachable", servers: 0, waiting: 4, enrolling: 0 })).toEqual({ kind: "unreachable", intake: true });
});

test("zero servers waits for Cloud before offering first run", () => {
  expect(serversPageState({ list: "live", servers: 0, waiting: undefined, enrolling: 0 })).toEqual({ kind: "loading", intake: false });
  expect(serversPageState({ list: "live", servers: 2, waiting: undefined, enrolling: undefined })).toEqual({ kind: "fleet", intake: false, stale: false });
});

test("the first server's enrollment shows its progress, not unreachable, before anything has joined", () => {
  expect(serversPageState({ list: "unreachable", servers: 0, waiting: 0, enrolling: 1, founding: true })).toEqual({ kind: "first-run", intake: true });
});

test("a server that joined and stopped answering is unreachable", () => {
  expect(serversPageState({ list: "unreachable", servers: 0, waiting: 0, enrolling: 1, founding: false })).toEqual({ kind: "unreachable", intake: true });
});

test("only an open first-server enrollment before the cluster is founded counts as founding", () => {
  const founder = { role: "founder" as const, endedAt: null };
  expect(foundingFirstServer({ founded: false, attempts: [founder] })).toBe(true);
  // A founder that resumes after founding, or an outage while it is still open, keeps the unreachable view.
  expect(foundingFirstServer({ founded: true, attempts: [founder] })).toBe(false);
  expect(foundingFirstServer({ founded: false, attempts: [{ role: "founder", endedAt: "2026-10-23T05:00:00.000Z" }] })).toBe(false);
  expect(foundingFirstServer({ founded: false, attempts: [{ role: "joiner", endedAt: null }] })).toBe(false);
  expect(foundingFirstServer(undefined)).toBe(false);
});

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

test("Get install command waits for Cloudflare, and a setup left half way falls back to public ports and says so", () => {
  expect(installGate({ at: "off", abandoned: false })).toEqual({ allowed: true, note: null, foundsClosed: false });
  expect(installGate({ at: "token" }).allowed).toBe(false);
  expect(installGate({ at: "zone", zones: ["zenploy.dev"] }).allowed).toBe(false);
  expect(installGate({ at: "ready", zone: "zenploy.dev" })).toEqual({ allowed: true, note: null, foundsClosed: true });
  const abandoned = abandonCloudflare({ at: "zone", zones: ["zenploy.dev"] });
  expect(abandoned).toEqual({ at: "off", abandoned: true });
  expect(installGate(abandoned)).toMatchObject({ allowed: true, foundsClosed: false, note: "Without Cloudflare Tunnel, your servers open ports 80 and 443." });
  expect(abandonCloudflare({ at: "off", abandoned: false })).toEqual({ at: "off", abandoned: false });
});
