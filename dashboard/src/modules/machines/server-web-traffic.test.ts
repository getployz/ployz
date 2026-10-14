import type { IngressCheck, MachineId } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { webTrafficChecked, webTrafficView } from "./server-web-traffic-view";
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
const view = (latest: LatestWebTraffic | null, over: Partial<typeof server> = {}, requested: { id: string; want: "on" | "off" } | null = null, settled = false) =>
  webTrafficView({ server: { ...server, ...over }, latest: latest === null ? {} : { m1: latest }, requested, settled });

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

  it("shows a failure only while the Server still disagrees with what it wanted", () => {
    const failed = change("failed", "on", { failure: "Port 80 is in use on web-1. Free it, then try again." });
    expect(view(failed)).toEqual({ kind: "failed", want: "on", reason: "Port 80 is in use on web-1. Free it, then try again.", address: "203.0.113.10" });
    expect(view(failed, { acceptsIngress: true })).toEqual({ kind: "on", address: "203.0.113.10" });
  });

  it("moves the switch only when the Server does", () => {
    expect(webTrafficChecked({ kind: "stopping" })).toBe(true);
    expect(webTrafficChecked({ kind: "starting" })).toBe(false);
    expect(webTrafficChecked({ kind: "failed", want: "on", reason: "x", address: null })).toBe(false);
    expect(webTrafficChecked({ kind: "failed", want: "off", reason: "x", address: null })).toBe(true);
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
