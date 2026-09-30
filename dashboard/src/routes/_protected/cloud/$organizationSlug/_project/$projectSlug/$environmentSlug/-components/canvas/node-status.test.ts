import type { DomainRow, ServiceListing } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import type { RuntimeContainerRecord } from "#/modules/runtime/runtime.collection";
import { deployTargeting, fillText, fillTone, nodeIssues, volumeFill, publicDomain, runtimeLine, stagedSurface, type NodeIssue, type RuntimeLens } from "./node-status";

const service: ServiceListing = { source: "image", change: null, template: null, id: "web", name: "web", private_dns: "web" };
const container = (state: "running" | "exited" | "restarting", health = "healthy"): RuntimeContainerRecord =>
  ({ id: state, displayName: "web", machineId: "m", namespace: "n", kind: "service", runtime: state === "running" ? { state, health }
    : state === "exited" ? { state, code: 1, stopped_at: null, oom_killed: false } : { state } });
const runtime = (...containers: RuntimeContainerRecord[]) => ({ containers });
const observed: RuntimeLens = { status: "observed", incomplete: false, observedAt: "2026-09-30T10:00:00Z", noServers: false };
const seen = { lens: observed, desiredReplicas: null, rolling: false, awaited: false };
/** The Runtime Watch saying something other than `observed`. */
const watch = (lens: Partial<RuntimeLens>) => ({ ...seen, lens: { ...observed, ...lens } });

describe("runtimeLine", () => {
  it("says what runs now in one word", () => {
    expect(runtimeLine(service, runtime(container("running", "healthy")), seen).word).toBe("Online");
    expect(runtimeLine(service, runtime(container("running", "unhealthy")), seen)).toMatchObject({ word: "Unhealthy", tone: "warn" });
    expect(runtimeLine(service, runtime(container("exited"), container("restarting")), seen)).toMatchObject({ word: "Crashed", tone: "crashed", down: true });
    expect(runtimeLine(service, runtime(), seen)).toMatchObject({ word: "Not running", tone: "bad", down: true });
  });

  it("says since when it crashed, from the newest stop, and Out of memory when that one was OOM-killed", () => {
    const stopped = (stopped_at: string | null, oom_killed: boolean): RuntimeContainerRecord =>
      ({ ...container("exited"), runtime: { state: "exited", code: 137, stopped_at, oom_killed } });
    expect(runtimeLine(service, runtime(stopped("2026-09-30T09:58:00.000Z", false), stopped("2026-09-30T09:50:00.000Z", true)), seen))
      .toEqual({ word: "Crashed", tone: "crashed", down: true, since: new Date("2026-09-30T09:58:00.000Z"), code: 137 });
    expect(runtimeLine(service, runtime(stopped("2026-09-30T09:50:00.000Z", false), stopped("2026-09-30T09:58:00.000Z", true)), seen))
      .toMatchObject({ word: "Out of memory", since: new Date("2026-09-30T09:58:00.000Z") });
    expect(runtimeLine(service, runtime(stopped(null, false)), seen)).toMatchObject({ word: "Crashed", since: null, code: 137 });
  });

  it("says a grey Starting while its containers run but none serves or fails a health check yet", () => {
    expect(runtimeLine(service, runtime(container("running", "starting")), seen)).toMatchObject({ word: "Starting", tone: "quiet", down: false });
  });

  it("says Not running once the Servers' whole evidence has none of it, and Deployed while a Server is missing from it", () => {
    expect(runtimeLine(service, null, seen)).toMatchObject({ word: "Not running", down: true });
    expect(runtimeLine(service, null, watch({ incomplete: true }))).toMatchObject({ word: "Deployed", down: false });
  });

  it("claims no crash from partial evidence: a replica may run on the Server that didn't report", () => {
    const partial = watch({ incomplete: true });
    expect(runtimeLine(service, runtime(container("exited")), partial)).toMatchObject({ word: "Not seen running", tone: "quiet", down: false });
    expect(nodeIssues(runtimeLine(service, runtime(container("exited")), partial), [], [])).toEqual([]);
  });

  it("says Starting, not Not running, while a Deploy in flight targets it and none of its containers exist yet", () => {
    const awaited = { ...seen, awaited: true };
    expect(runtimeLine(service, null, awaited)).toMatchObject({ word: "Starting", tone: "quiet", down: false });
    expect(runtimeLine(service, runtime(), awaited)).toMatchObject({ word: "Starting", down: false });
    expect(runtimeLine(service, runtime(), seen).word).toBe("Not running");
    expect(runtimeLine(service, runtime(container("exited")), awaited).word).toBe("Crashed");
  });

  it("never guesses without current evidence: it waits, greys the last word with its age, or says why", () => {
    const since = new Date("2026-09-30T10:00:00Z");
    expect(runtimeLine(service, null, watch({ status: "connecting", observedAt: null })).tone).toBe("pending");
    expect(runtimeLine(service, runtime(container("exited")), watch({ status: "unavailable" }))).toEqual({ word: "Crashed", tone: "quiet", down: false, since, code: 1 });
    expect(runtimeLine(service, null, watch({ status: "unavailable" }))).toEqual({ word: "Not running", tone: "quiet", down: false, since, code: null });
    expect(runtimeLine(service, null, watch({ status: "unavailable", observedAt: null })).tone).toBe("pending");
    expect(runtimeLine(service, null, watch({ status: "unreachable" }))).toMatchObject({ word: "Can't reach servers", tone: "unreachable" });
    expect(runtimeLine(service, null, watch({ status: "no_connection", noServers: true }))).toMatchObject({ word: "Needs a server", down: false });
  });

  it("says Not deployed for a new Service and No source for an empty one, whatever runs", () => {
    expect(runtimeLine({ ...service, change: "create" }, null, seen).word).toBe("Not deployed");
    expect(runtimeLine({ ...service, source: "empty" }, null, seen).word).toBe("No source");
    expect(runtimeLine({ ...service, change: "create" }, null, watch({ status: "unreachable" })).word).toBe("Not deployed");
  });

  it("says Degraded when fewer replicas serve than it asks for, except while a Deploy rolls them or a Server didn't report", () => {
    const one = runtime(container("running", "healthy"), container("exited"));
    expect(runtimeLine(service, one, { ...seen, desiredReplicas: 2 }).word).toBe("Degraded");
    expect(runtimeLine(service, one, { ...seen, desiredReplicas: 2, rolling: true }).word).toBe("Online");
    expect(runtimeLine(service, one, { ...seen, desiredReplicas: 1 }).word).toBe("Online");
    expect(runtimeLine(service, one, { ...watch({ incomplete: true }), desiredReplicas: 2 }).word).toBe("Online");
  });
});

describe("nodeIssues", () => {
  const domain = (status: DomainRow["status"]): DomainRow =>
    ({ kind: "custom", hostname: `${status}.com`, service: "web", port: null, status, reason: null, action: null });
  const kinds = (issues: NodeIssue[]) => issues.map((issue) => issue.kind);

  it("lists what the user can fix: a Service down or struggling, then domains that need them", () => {
    const crashed = runtimeLine(service, runtime(container("exited")), seen);
    expect(nodeIssues(crashed, [domain("needs_attention")], [])).toEqual([{ kind: "runtime", line: crashed }, { kind: "domain", domain: domain("needs_attention") }]);
    expect(kinds(nodeIssues(runtimeLine(service, runtime(), seen), [], []))).toEqual(["runtime"]);
    expect(kinds(nodeIssues(runtimeLine(service, runtime(container("running", "unhealthy")), seen), [], []))).toEqual(["runtime"]);
    expect(kinds(nodeIssues(runtimeLine(service, runtime(container("running", "healthy")), seen), [domain("needs_attention"), domain("setting_up")], [])))
      .toEqual(["domain"]);
  });

  it("lists nothing for a healthy, starting, new or empty Service, nor a grey word", () => {
    expect(nodeIssues(runtimeLine(service, runtime(container("running", "not_configured")), seen), [domain("ready")], [])).toEqual([]);
    expect(nodeIssues(runtimeLine(service, runtime(container("running", "starting")), seen), [], [])).toEqual([]);
    expect(nodeIssues(runtimeLine(service, runtime(container("exited")), watch({ status: "unavailable" })), [], [])).toEqual([]);
    expect(nodeIssues(runtimeLine({ ...service, change: "create" }, null, seen), [], [])).toEqual([]);
  });
});

describe("volume fill", () => {
  const usage = (name: string, usedBytes: number) => ({ name, usedBytes, boundBytes: 100 });

  it("is the fullest Server's share of the Volume's bound, none without a bound", () => {
    expect(volumeFill([usage("ns_vol-a", 40), usage("ns_vol-a", 92), usage("ns_vol-b", 99)], "ns_vol-a")).toBe(0.92);
    expect(volumeFill([usage("ns_vol-b", 99)], "ns_vol-a")).toBeNull();
    expect(volumeFill([usage("ns_vol-a", 150)], "ns_vol-a")).toBe(1);
  });

  it("turns amber from 80% and red from 95%, and each is an issue", () => {
    expect([0.79, 0.8, 0.94, 0.95, null].map(fillTone)).toEqual([null, "warn", "warn", "bad", null]);
    expect(fillText(0.926)).toBe("92% full");
    const online = runtimeLine(service, runtime(container("running", "healthy")), seen);
    const volume = (name: string) => ({ id: name, name, storage_locked: true });
    expect(nodeIssues(online, [], [0.5, 0.85, 0.97, null].map((fill, i) => ({ volume: volume(`v${i}`), fill }))))
      .toEqual([{ kind: "volume", volume: volume("v1"), fill: 0.85, tone: "warn" }, { kind: "volume", volume: volume("v2"), fill: 0.97, tone: "bad" }]);
  });
});

describe("publicDomain", () => {
  const generated = { kind: "generated", prefix: "web", hostname: "web.acme.ployz.app", service: "web", port: null, status: "ready", reason: null, action: null } as const;
  const custom = { kind: "custom", hostname: "acme.com", service: "web", port: null, status: "setting_up", reason: null, action: null } as const;

  it("is the first custom domain, else the generated one, muted until it's set up", () => {
    expect(publicDomain([generated, custom])).toEqual({ hostname: "acme.com", live: false });
    expect(publicDomain([generated])).toEqual({ hostname: "web.acme.ployz.app", live: true });
    expect(publicDomain([{ ...custom, status: "needs_attention" }])).toEqual({ hostname: "acme.com", live: true });
    expect(publicDomain([{ ...generated, hostname: null }])).toBeNull();
  });
});

describe("deployTargeting", () => {
  const deployment = (status: "queued" | "running", nodes: string[] | null, services: string[] = []) =>
    ({ status, services, nodes, started_at: status === "running" ? 100 : null, admitted_at: 90 });

  it("puts a Deploy in flight that targets the Service first: running, else queued", () => {
    expect(deployTargeting({ ...service, change: "update" }, 2, [deployment("queued", ["web"]), deployment("running", ["web"])]).chip).toEqual({ kind: "deploying", since: 100 });
    expect(deployTargeting(service, 0, [deployment("queued", ["web"])]).chip).toEqual({ kind: "queued" });
    expect(deployTargeting(service, 0, [deployment("running", ["web"])]).rolling).toBe(true);
    expect(deployTargeting(service, 0, [deployment("queued", ["web"])]).rolling).toBe(false);
    expect(deployTargeting(service, 0, [deployment("running", ["api"])]).chip).toBeNull();
  });

  it("targets nothing while a Deploy's nodes haven't arrived", () => {
    expect(deployTargeting(service, 0, [deployment("running", null)]).chip).toBeNull();
    expect(deployTargeting({ ...service, change: "update" }, 1, [deployment("queued", null)]).chip).toEqual({ kind: "staged", label: "1 change", variant: "info" });
  });

  it("awaits a Deploy whose nodes haven't arrived when it names no Services or names this one, so a first Deploy reads Starting", () => {
    expect(deployTargeting(service, 0, [deployment("running", null)]).awaited).toBe(true);
    expect(deployTargeting(service, 0, [deployment("queued", null, ["web"])]).awaited).toBe(true);
    expect(deployTargeting(service, 0, [deployment("running", null, ["api"])]).awaited).toBe(false);
    expect(deployTargeting(service, 0, [deployment("running", ["api"])]).awaited).toBe(false);
    expect(deployTargeting(service, 0, [deployment("running", ["web"])]).awaited).toBe(true);
  });

  it("says what the next Deploy does otherwise", () => {
    expect(deployTargeting({ ...service, change: "create" }, 0, []).chip).toEqual({ kind: "staged", label: "New", variant: "success" });
    expect(deployTargeting({ ...service, change: "update" }, 1, []).chip).toEqual({ kind: "staged", label: "1 change", variant: "info" });
    expect(deployTargeting({ ...service, change: "update" }, 3, []).chip).toEqual({ kind: "staged", label: "3 changes", variant: "info" });
    expect(deployTargeting({ ...service, change: "update" }, 0, []).chip).toEqual({ kind: "staged", label: "Changed", variant: "info" });
    expect(deployTargeting({ ...service, change: "delete" }, 0, []).chip).toEqual({ kind: "staged", label: "Removing", variant: "destructive" });
  });
});

describe("stagedSurface", () => {
  it("is green for a new node, blue for a change and red for a removal, and none while a Deployment Page is open", () => {
    expect(stagedSurface(undefined, "create")).toBe("success");
    expect(stagedSurface(undefined, "update")).toBe("info");
    expect(stagedSurface(undefined, "delete")).toBe("destructive");
    expect(stagedSurface(undefined, null)).toBeUndefined();
    expect(stagedSurface(null, "delete")).toBeUndefined();
  });
});
