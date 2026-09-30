import type { DomainRow, ServiceListing } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import type { RuntimeContainerRecord } from "#/modules/runtime/runtime.collection";
import { deployChip, nodeIssues, publicDomain, runtimeLine, stagedSurface, type RuntimeLens } from "./node-status";

const service: ServiceListing = { source: "image", change: null, template: null, id: "web", name: "web", private_dns: "web" };
const container = (state: string, health?: string): RuntimeContainerRecord =>
  ({ id: state, displayName: "web", machineId: "m", namespace: "n", kind: "service", runtime: health ? { state, health } : { state } });
const runtime = (...containers: RuntimeContainerRecord[]) => ({ containers });
const observed: RuntimeLens = { status: "observed", incomplete: false, observedAt: "2026-09-30T10:00:00Z", noServers: false };
const seen = { lens: observed, desiredReplicas: null, chip: null };
/** The Runtime Watch saying something other than `observed`. */
const watch = (lens: Partial<RuntimeLens>) => ({ ...seen, lens: { ...observed, ...lens } });

describe("runtimeLine", () => {
  it("says what runs now in one word", () => {
    expect(runtimeLine(service, runtime(container("running", "healthy")), seen).word).toBe("Online");
    expect(runtimeLine(service, runtime(container("running", "unhealthy")), seen)).toMatchObject({ word: "Unhealthy", tone: "warn" });
    expect(runtimeLine(service, runtime(container("exited"), container("restarting")), seen)).toMatchObject({ word: "Crashed", tone: "crashed", down: true });
    expect(runtimeLine(service, runtime(), seen)).toMatchObject({ word: "Not running", tone: "bad", down: true });
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
    expect(nodeIssues(runtimeLine(service, runtime(container("exited")), partial), [])).toBeNull();
  });

  it("says Starting, not Not running, while a Deploy in flight targets it and none of its containers exist yet", () => {
    for (const chip of [{ kind: "deploying", since: 1 }, { kind: "queued" }] as const) {
      expect(runtimeLine(service, null, { ...seen, chip })).toMatchObject({ word: "Starting", tone: "quiet", down: false });
      expect(runtimeLine(service, runtime(), { ...seen, chip })).toMatchObject({ word: "Starting", down: false });
    }
    expect(runtimeLine(service, runtime(), { ...seen, chip: { kind: "staged", label: "Changed", variant: "info" } }).word).toBe("Not running");
    expect(runtimeLine(service, runtime(container("exited")), { ...seen, chip: { kind: "queued" } }).word).toBe("Crashed");
  });

  it("never guesses without current evidence: it waits, greys the last word with its age, or says why", () => {
    const since = new Date("2026-09-30T10:00:00Z");
    expect(runtimeLine(service, null, watch({ status: "connecting", observedAt: null })).tone).toBe("pending");
    expect(runtimeLine(service, runtime(container("exited")), watch({ status: "unavailable" }))).toEqual({ word: "Crashed", tone: "quiet", down: false, since });
    expect(runtimeLine(service, null, watch({ status: "unavailable" }))).toEqual({ word: "Not running", tone: "quiet", down: false, since });
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
    expect(runtimeLine(service, one, { ...seen, desiredReplicas: 2, chip: { kind: "deploying", since: 1 } }).word).toBe("Online");
    expect(runtimeLine(service, one, { ...seen, desiredReplicas: 1 }).word).toBe("Online");
    expect(runtimeLine(service, one, { ...watch({ incomplete: true }), desiredReplicas: 2 }).word).toBe("Online");
  });
});

describe("nodeIssues", () => {
  const domain = (status: DomainRow["status"]) => ({ status });

  it("counts what the user can fix, red when a Service is down", () => {
    expect(nodeIssues(runtimeLine(service, runtime(container("exited")), seen), [domain("needs_attention")])).toEqual({ count: 2, tone: "bad" });
    expect(nodeIssues(runtimeLine(service, runtime(), seen), [])).toEqual({ count: 1, tone: "bad" });
    expect(nodeIssues(runtimeLine(service, runtime(container("running", "healthy")), seen), [domain("needs_attention"), domain("setting_up")]))
      .toEqual({ count: 1, tone: "warn" });
  });

  it("counts nothing for a healthy, starting, new or empty Service, nor a grey word", () => {
    expect(nodeIssues(runtimeLine(service, runtime(container("running", "not_configured")), seen), [domain("ready")])).toBeNull();
    expect(nodeIssues(runtimeLine(service, runtime(container("running", "starting")), seen), [])).toBeNull();
    expect(nodeIssues(runtimeLine(service, runtime(container("exited")), watch({ status: "unavailable" })), [])).toBeNull();
    expect(nodeIssues(runtimeLine({ ...service, change: "create" }, null, seen), [])).toBeNull();
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

describe("deployChip", () => {
  const deployment = (status: "queued" | "running", services: string[], nodes: string[] | null = null) =>
    ({ status, services, nodes, started_at: status === "running" ? 100 : null, admitted_at: 90 });

  it("puts a Deploy in flight first: running, else queued, when it targets the Service or every Service", () => {
    expect(deployChip({ ...service, change: "update" }, 2, [deployment("queued", []), deployment("running", ["web"])])).toEqual({ kind: "deploying", since: 100 });
    expect(deployChip(service, 0, [deployment("queued", ["web"])])).toEqual({ kind: "queued" });
    expect(deployChip(service, 0, [deployment("running", ["api"])])).toBeNull();
  });

  it("follows the nodes a Deploy targets once its view arrives: a Service created after it gets no chip", () => {
    expect(deployChip(service, 0, [deployment("running", [], ["api"])])).toBeNull();
    expect(deployChip(service, 0, [deployment("running", [], ["web", "api"])])).toEqual({ kind: "deploying", since: 100 });
  });

  it("says what the next Deploy does otherwise", () => {
    expect(deployChip({ ...service, change: "create" }, 0, [])).toEqual({ kind: "staged", label: "New", variant: "success" });
    expect(deployChip({ ...service, change: "update" }, 1, [])).toEqual({ kind: "staged", label: "1 change", variant: "info" });
    expect(deployChip({ ...service, change: "update" }, 3, [])).toEqual({ kind: "staged", label: "3 changes", variant: "info" });
    expect(deployChip({ ...service, change: "update" }, 0, [])).toEqual({ kind: "staged", label: "Changed", variant: "info" });
    expect(deployChip({ ...service, change: "delete" }, 0, [])).toEqual({ kind: "staged", label: "Removing", variant: "destructive" });
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
