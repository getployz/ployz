import type { IngressCheck, MachineId } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { ingressProxyOn, settleLeft, WEB_TRAFFIC_SETTLE_MS, webTrafficChecked, webTrafficReach, webTrafficView, type IngressProxy } from "./server-web-traffic-view";
import { type LatestWebTraffic, startFailure } from "./server-web-traffic";

const server = { id: "m1", acceptsIngress: false, publicIp: "203.0.113.10" };
const change = (
  state: LatestWebTraffic["state"],
  want: LatestWebTraffic["want"],
  over: { readonly attemptId?: string; readonly failure?: string } = {},
): LatestWebTraffic => {
  const base = { attemptId: over.attemptId ?? "a1", want };
  switch (state) {
    case "pending":
      return { ...base, state, requestedAt: "t" };
    case "running":
      return { ...base, state, startedAt: "t" };
    case "finished":
      return { ...base, state, endedAt: "2000-01-01T00:00:00Z" };
    case "failed":
      return { ...base, state, endedAt: "2000-01-01T00:00:00Z", failure: over.failure ?? "why" };
  }
};
const view = (
  latest: LatestWebTraffic | null,
  over: Partial<typeof server> = {},
  requested: { id: string; want: "on" | "off" } | null = null,
  settled = false,
  proxy: IngressProxy = "running",
) => webTrafficView({ server: { ...server, ...over }, latest: latest === null ? {} : { m1: latest }, requested, settled, proxy });

describe("webTrafficView", () => {
  it("reads the Server when nothing was ever asked", () => {
    expect(view(null)).toEqual({ kind: "off" });
    expect(view(null, { acceptsIngress: true })).toEqual({ kind: "on", address: "203.0.113.10" });
  });

  it("shows this tab's flip under way until its row arrives", () => {
    expect(view(null, {}, { id: "r1", want: "on" })).toEqual({ kind: "starting" });
    expect(view(change("pending", "on", { attemptId: "r1" }), {}, { id: "r1", want: "on" })).toEqual({ kind: "starting" });
    expect(view(change("running", "off"), { acceptsIngress: true })).toEqual({ kind: "stopping" });
  });

  it("keeps a finished change showing until the Server shows it or it settles", () => {
    const done = change("finished", "on");
    expect(view(done)).toEqual({ kind: "starting" });
    expect(view(done, {}, null, true)).toEqual({ kind: "off" });
    expect(view(done, { acceptsIngress: true })).toEqual({ kind: "on", address: "203.0.113.10" });
  });

  it("shows a failed start whichever way the Server ended up, so a role left on never reads as healthy", () => {
    const reason = "Port 80 is in use on web-1. Free it, then try again.";
    const failed = change("failed", "on", { failure: reason });
    expect(view(failed)).toEqual({ kind: "failed", want: "on", on: false, reason, address: null });
    expect(view(failed, { acceptsIngress: true })).toEqual({ kind: "failed", want: "on", on: true, reason, address: "203.0.113.10" });
  });

  it("shows a failed stop even once the role is off, since the web server may still run", () => {
    const failed = change("failed", "off");
    expect(view(failed, { acceptsIngress: true })).toEqual({ kind: "failed", want: "off", on: true, reason: "why", address: "203.0.113.10" });
    expect(view(failed)).toEqual({ kind: "failed", want: "off", on: false, reason: "why", address: null });
  });

  it("never claims a start failed from the runtime alone, since a start under way shows no web server either", () => {
    const down = { kind: "down" };
    expect(view(null, { acceptsIngress: true }, null, false, "down")).toEqual(down);
    expect(view(change("finished", "off"), { acceptsIngress: true }, null, true, "down")).toEqual(down);
    expect(view(change("failed", "off"), {}, null, true, "down")).toEqual({ kind: "failed", want: "off", on: false, reason: "why", address: null });
    expect(view(null, { acceptsIngress: true }, null, false, "unknown")).toEqual({ kind: "on", address: "203.0.113.10" });
    expect(view(null, {}, null, false, "down")).toEqual({ kind: "off" });
    expect(view(change("running", "on"), { acceptsIngress: true }, null, false, "down")).toEqual({ kind: "starting" });
  });

  it("keeps a finished start starting until its web server shows running, so it never flashes as down", () => {
    const done = change("finished", "on");
    expect(view(done, { acceptsIngress: true }, null, false, "down")).toEqual({ kind: "starting" });
    expect(view(done, { acceptsIngress: true }, null, false, "running")).toEqual({ kind: "on", address: "203.0.113.10" });
    expect(view(done, { acceptsIngress: true }, null, true, "down")).toEqual({ kind: "down" });
  });

  it("says your domains point here after a failed change only while the web server may serve them", () => {
    const failed = change("failed", "on", { failure: "Port 80 is in use on web-1. Free it, then try again." });
    expect(view(failed, { acceptsIngress: true }, null, false, "down")).toMatchObject({ kind: "failed", on: true, address: null });
    expect(view(failed, { acceptsIngress: true }, null, false, "running")).toMatchObject({ kind: "failed", on: true, address: "203.0.113.10" });
    expect(view(change("failed", "off"), { acceptsIngress: true }, null, false, "running")).toMatchObject({ on: true, address: "203.0.113.10" });
  });

  it("moves the switch only when the Server does", () => {
    expect(webTrafficChecked({ kind: "stopping" })).toBe(true);
    expect(webTrafficChecked({ kind: "starting" })).toBe(false);
    expect(webTrafficChecked({ kind: "failed", want: "on", on: false, reason: "x", address: null })).toBe(false);
    expect(webTrafficChecked({ kind: "failed", want: "on", on: true, reason: "x", address: null })).toBe(true);
    expect(webTrafficChecked({ kind: "down" })).toBe(true);
  });
});

describe("settleLeft", () => {
  const ended = "2026-01-01T12:00:00.000Z";
  const at = (iso: string) => new Date(iso).getTime();

  it("counts the settle time from when the change ended", () => {
    expect(settleLeft(ended, at("2026-01-01T12:00:10.000Z"))).toBe(WEB_TRAFFIC_SETTLE_MS - 10_000);
    expect(settleLeft(ended, at("2026-01-01T12:01:00.000Z"))).toBeLessThanOrEqual(0);
  });

  it("never waits longer than the settle time when the browser's clock is behind Cloud's", () => {
    expect(settleLeft(ended, at("2026-01-01T11:55:00.000Z"))).toBe(WEB_TRAFFIC_SETTLE_MS);
  });
});

describe("ingressProxyOn", () => {
  const ingress = (...copies: (readonly [machineId: string, state: string])[]) => ({
    containers: copies.map(([machineId, state]) =>
      ({ id: `c-${machineId}`, displayName: "ingress", machineId, namespace: "ployz-system", kind: "service_container", runtime: { state } })),
  });
  const complete = { machines: [], containers: [] };

  it("is running only while this Server's Ingress Proxy container runs", () => {
    expect(ingressProxyOn("m1", ingress(["m1", "running"]), complete)).toBe("running");
    expect(ingressProxyOn("m1", ingress(["m1", "restarting"]), complete)).toBe("down");
    expect(ingressProxyOn("m1", ingress(["m2", "running"]), complete)).toBe("down");
    expect(ingressProxyOn("m1", null, complete)).toBe("down");
  });

  it("is unknown while this Server or any container went unobserved, unless it saw this one running", () => {
    expect(ingressProxyOn("m1", null, { machines: ["m1"], containers: [] })).toBe("unknown");
    expect(ingressProxyOn("m1", null, { machines: [], containers: ["c9"] })).toBe("unknown");
    expect(ingressProxyOn("m1", ingress(["m1", "running"]), { machines: [], containers: ["c9"] })).toBe("running");
  });
});

describe("startFailure", () => {
  const repair = (failure: IngressCheck["repairs"][number]["failure"]): IngressCheck =>
    ({ repairs: [{ machine: "m1" as MachineId, server: "web-1", action: "create", failure }], unchecked: [] });

  it("names the port another process holds", () => {
    expect(startFailure(repair({ type: "port_taken", cause: "listen tcp4 0.0.0.0:80: bind: address already in use" }), "m1"))
      .toBe("Port 80 is in use on web-1. Free it, then try again.");
    expect(startFailure(repair({ type: "port_taken", cause: "Bind for 0.0.0.0:80 failed: port is already allocated" }), "m1"))
      .toBe("Port 80 is in use on web-1. Free it, then try again.");
    expect(startFailure(repair({ type: "port_taken", cause: "port is already allocated" }), "m1"))
      .toBe("The web port is in use on web-1. Free it, then try again.");
  });

  it("passes other causes through, and is quiet when it started", () => {
    expect(startFailure(repair({ type: "failed", cause: "no such image" }), "m1")).toBe("The web server didn't start on web-1: no such image");
    expect(startFailure(repair(null), "m1")).toBeNull();
    expect(startFailure({ repairs: [], unchecked: ["web-1 didn't answer"] }, "m1")).toBe("Couldn't check whether the web server started. Try again.");
    expect(startFailure({ repairs: [], unchecked: [] }, "m1")).toBeNull();
  });
});

describe("webTrafficReach", () => {
  const here = { id: "m1", acceptsIngress: true, publicIp: "203.0.113.10" };
  const other = (over: { readonly publicIp?: string | null; readonly status?: "online" | "offline" } = {}) => ({
    machine: { id: "m2", acceptsIngress: true, publicIp: over.publicIp === undefined ? "203.0.113.11" : over.publicIp },
    status: over.status ?? ("online" as const),
  });

  it("is last when no other online Server with an address takes web traffic", () => {
    expect(webTrafficReach(here, [{ machine: here, status: "online" }, other({ status: "offline" })])).toBe("last");
    expect(webTrafficReach(here, [other({ publicIp: null })])).toBe("last");
  });

  it("is shared while another online Server serves", () => {
    expect(webTrafficReach(here, [other()])).toBe("shared");
  });

  it("is away without a public IP or once its role is off, since it no longer takes web traffic", () => {
    expect(webTrafficReach({ ...here, publicIp: null }, [])).toBe("away");
    expect(webTrafficReach({ ...here, acceptsIngress: false }, [])).toBe("away");
  });
});
