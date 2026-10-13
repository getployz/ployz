import type { DomainRow, NodeOutcome, RowId, ServiceListing } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import type { RuntimeContainerRecord } from "#/modules/runtime/runtime.collection";
import { asTestDouble } from "#/lib/test-double";
import { containerTrouble, deployChip, nodeIssues, notSetLabel, publicDomain, restartedServices, runtimeLine, stagedSurface, type RuntimeLens } from "./node-status";

const service: ServiceListing = { source: "image", change: null, template: null, id: "web", row: "web:node" as RowId, name: "web", private_dns: "web" };
const container = (state: string, health?: string): RuntimeContainerRecord =>
  ({ id: state, displayName: "web", machineId: "m", namespace: "n", kind: "service", runtime: health ? { state, health } : { state } });
const runtime = (...containers: RuntimeContainerRecord[]) => ({ containers });
const observed: RuntimeLens = { status: "observed", incomplete: false, observedAt: "2026-09-30T10:00:00Z", noServers: false };
const seen = { lens: observed, desiredReplicas: null, deploying: false };
/** The Runtime Watch saying something other than `observed`. */
const watch = (lens: Partial<RuntimeLens>) => ({ ...seen, lens: { ...observed, ...lens } });

describe("runtimeLine", () => {
  it("says what runs now in one word", () => {
    expect(runtimeLine(service, runtime(container("running", "healthy")), seen).word).toBe("Online");
    expect(runtimeLine(service, runtime(container("exited"), container("restarting")), seen)).toMatchObject({ word: "Crashed", tone: "crashed", down: true });
    expect(runtimeLine(service, runtime(), seen)).toMatchObject({ word: "Not running", tone: "bad", down: true });
  });

  it("says a grey Starting while its containers run but none has passed its healthcheck yet", () => {
    expect(runtimeLine(service, runtime(container("running", "starting")), seen)).toMatchObject({ word: "Starting", tone: "quiet", down: false });
    expect(runtimeLine(service, runtime(container("running", "unhealthy")), seen)).toMatchObject({ word: "Starting", tone: "quiet", down: false, checkFailing: false });
  });

  it("stays Online with its check failing while a replica that passed it once fails it now", () => {
    expect(runtimeLine(service, runtime(container("running", "failing")), seen)).toMatchObject({ word: "Online", tone: "ok", checkFailing: true });
    expect(runtimeLine(service, runtime(container("running", "healthy"), container("running", "failing")), seen))
      .toMatchObject({ word: "Online", checkFailing: true });
    expect(runtimeLine(service, runtime(container("running", "healthy")), seen).checkFailing).toBe(false);
  });

  it("distinguishes clean exits from failure exits and crash loops without hiding downtime", () => {
    const exited = (code: number) => ({ ...container("exited"), runtime: { state: "exited", code } });
    expect(runtimeLine(service, runtime(exited(0)), seen)).toMatchObject({ word: "Stopped", tone: "bad", down: true });
    expect(runtimeLine(service, runtime(exited(0), exited(1)), seen)).toMatchObject({ word: "Crashed", tone: "crashed", down: true });
    expect(runtimeLine(service, runtime(exited(0), container("restarting")), seen).word).toBe("Crashed");
    expect(runtimeLine(service, runtime(exited(0)), watch({ status: "unavailable" }))).toMatchObject({ word: "Stopped", tone: "quiet", down: false });
  });

  it("says Not running once the Servers' whole evidence has none of it, and Deployed while a Server is missing from it", () => {
    expect(runtimeLine(service, null, seen)).toMatchObject({ word: "Not running", down: true });
    expect(runtimeLine(service, null, watch({ incomplete: true }))).toMatchObject({ word: "Deployed", down: false });
  });

  it("never guesses without current evidence: it waits, greys the last word with its age, or says why", () => {
    const since = new Date("2026-09-30T10:00:00Z");
    expect(runtimeLine(service, null, watch({ status: "connecting", observedAt: null })).tone).toBe("pending");
    expect(runtimeLine(service, runtime(container("exited")), watch({ status: "unavailable" }))).toEqual({ word: "Crashed", tone: "quiet", down: false, since, checkFailing: false });
    expect(runtimeLine(service, null, watch({ status: "unavailable" }))).toEqual({ word: "Not running", tone: "quiet", down: false, since, checkFailing: false });
    expect(runtimeLine(service, null, watch({ status: "unavailable", observedAt: null }))).toEqual({ word: "Status unavailable", tone: "quiet", down: false, since: null, checkFailing: false });
    expect(runtimeLine(service, null, watch({ status: "unreachable" }))).toMatchObject({ word: "Can't reach servers", tone: "unreachable" });
    expect(runtimeLine(service, null, watch({ status: "no_connection", noServers: true }))).toMatchObject({ word: "Needs a server", down: false });
  });

  it("says Not deployed for a new Service and No source for an empty one, whatever runs", () => {
    expect(runtimeLine({ ...service, change: "create" }, null, seen).word).toBe("Not deployed");
    expect(runtimeLine({ ...service, source: "empty" }, null, seen).word).toBe("No source");
    expect(runtimeLine({ ...service, change: "create" }, null, watch({ status: "unreachable" })).word).toBe("Not deployed");
  });

  it("says what runs for a new Service a failed Deploy still started", () => {
    expect(runtimeLine({ ...service, change: "create" }, runtime(container("restarting")), seen)).toMatchObject({ word: "Crashed", down: true });
  });

  it("says Degraded when fewer replicas serve than it asks for, except while a Deploy rolls them or a Server didn't report", () => {
    const one = runtime(container("running", "healthy"), container("exited"));
    expect(runtimeLine(service, one, { ...seen, desiredReplicas: 2 }).word).toBe("Degraded");
    expect(runtimeLine(service, one, { ...seen, desiredReplicas: 2, deploying: true }).word).toBe("Online");
    expect(runtimeLine(service, one, { ...seen, desiredReplicas: 1 }).word).toBe("Online");
    expect(runtimeLine(service, one, { ...watch({ incomplete: true }), desiredReplicas: 2 }).word).toBe("Online");
  });
});

describe("nodeIssues", () => {
  const domain = (status: DomainRow["status"], hostname: string | null = "acme.com") => ({ status, hostname });

  it("counts what the user can fix, red when a Service is down", () => {
    expect(nodeIssues(runtimeLine(service, runtime(container("exited")), seen), [domain("needs_attention")])).toEqual({ count: 2, tone: "bad", monitoring: false, trouble: null });
    expect(nodeIssues(runtimeLine(service, runtime(), seen), [])).toEqual({ count: 1, tone: "bad", monitoring: false, trouble: null });
    expect(nodeIssues(runtimeLine(service, runtime(container("running", "healthy")), seen), [domain("needs_attention"), domain("setting_up")]))
      .toEqual({ count: 1, tone: "warn", monitoring: false, trouble: null });
  });

  it("counts one amber warning for a failing healthcheck, however many replicas fail it", () => {
    const failing = runtime(container("running", "failing"), container("running", "failing"));
    expect(nodeIssues(runtimeLine(service, failing, seen), [])).toEqual({ count: 1, tone: "warn", monitoring: false, trouble: null });
    expect(nodeIssues(runtimeLine(service, failing, seen), [domain("needs_attention")])).toEqual({ count: 2, tone: "warn", monitoring: false, trouble: null });
    expect(nodeIssues(runtimeLine(service, failing, watch({ status: "unavailable" })), [])).toBeNull();
  });

  it("counts nothing for a healthy, starting, new or empty Service, nor a grey word", () => {
    expect(nodeIssues(runtimeLine(service, runtime(container("running", "not_configured")), seen), [domain("ready")])).toBeNull();
    expect(nodeIssues(runtimeLine(service, runtime(container("running", "starting")), seen), [])).toBeNull();
    expect(nodeIssues(runtimeLine(service, runtime(container("exited")), watch({ status: "unavailable" })), [])).toBeNull();
    expect(nodeIssues(runtimeLine({ ...service, change: "create" }, null, seen), [])).toBeNull();
  });

  describe("with an open uptime incident", () => {
    const healthy = runtimeLine(service, runtime(container("running", "healthy")), seen);

    it("adds one red issue", () => {
      expect(nodeIssues(healthy, [], { unreachable: "acme.com" })).toEqual({ count: 1, tone: "bad", monitoring: true, trouble: null });
      expect(nodeIssues(runtimeLine(service, runtime(container("running", "failing")), seen), [], { unreachable: "acme.com" })).toEqual({ count: 2, tone: "bad", monitoring: true, trouble: null });
    });

    it("adds nothing when the Service is already down", () => {
      expect(nodeIssues(runtimeLine(service, runtime(), seen), [], { unreachable: "acme.com" })).toEqual({ count: 1, tone: "bad", monitoring: false, trouble: null });
    });

    it("shares one issue with a domain needing attention on the same hostname", () => {
      expect(nodeIssues(healthy, [domain("needs_attention")], { unreachable: "acme.com" })).toEqual({ count: 1, tone: "bad", monitoring: false, trouble: null });
      expect(nodeIssues(healthy, [domain("needs_attention", "web.acme.ployz.app")], { unreachable: "acme.com" })).toEqual({ count: 2, tone: "bad", monitoring: true, trouble: null });
    });
  });

  describe("with an OOM kill or restart in the last hour", () => {
    const healthy = runtimeLine(service, runtime(container("running", "healthy")), seen);

    it("adds one amber issue named for it, which opens Monitoring", () => {
      expect(nodeIssues(healthy, [], { trouble: "OOM" })).toEqual({ count: 1, tone: "warn", monitoring: true, trouble: "OOM" });
    });

    it("counts beside a Service that's down, which stays red", () => {
      expect(nodeIssues(runtimeLine(service, runtime(container("exited")), seen), [], { trouble: "Restarting" }))
        .toEqual({ count: 2, tone: "bad", monitoring: true, trouble: "Restarting" });
    });
  });
});

describe("containerTrouble", () => {
  const web1 = { id: "m1", name: "web-1" };
  const web2 = { id: "m2", name: "web-2" };
  const now = Date.UTC(2026, 9, 12, 12);
  const marker = (kind: "oom" | "exit" | "restart", replica: string, server = web1, at = now - 60_000) => ({ kind, replica, server, at });

  it("says nothing when each replica restarted once, as a requested restart or rollback does", () => {
    expect(containerTrouble([marker("restart", "web-1"), marker("restart", "web-2"), marker("restart", "web-3")], now)).toBeNull();
  });

  it("says Restarting when one replica restarted twice", () => {
    expect(containerTrouble([marker("restart", "web-1"), marker("restart", "web-2"), marker("restart", "web-1")], now)).toBe("Restarting");
  });

  it("tells apart same-named replicas on different Servers", () => {
    expect(containerTrouble([marker("restart", "web-1", web1), marker("restart", "web-1", web2)], now)).toBeNull();
  });

  it("says OOM for one OOM kill, over any restarts, and nothing for exits alone", () => {
    expect(containerTrouble([marker("oom", "web-1")], now)).toBe("OOM");
    expect(containerTrouble([marker("restart", "web-1"), marker("restart", "web-1"), marker("oom", "web-2")], now)).toBe("OOM");
    expect(containerTrouble([marker("exit", "web-1"), marker("exit", "web-1")], now)).toBeNull();
    expect(containerTrouble([], now)).toBeNull();
  });

  it("counts only markers from the hour before now", () => {
    const hour = 60 * 60_000;
    expect(containerTrouble([marker("oom", "web-1", web1, now - hour + 1)], now)).toBe("OOM");
    expect(containerTrouble([marker("oom", "web-1", web1, now - hour)], now)).toBeNull();
    expect(containerTrouble([marker("restart", "web-1", web1, now - 2 * hour), marker("restart", "web-1")], now)).toBeNull();
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
  const deployment = (status: "queued" | "running", services: string[]) => ({ status, services, started_at: status === "running" ? 100 : null, admitted_at: 90 });

  it("puts a Deploy in flight first: running, else queued, when it targets the Service or every Service", () => {
    expect(deployChip({ ...service, change: "update" }, 2, [deployment("queued", []), deployment("running", ["web"])])).toEqual({ kind: "deploying", since: 100 });
    expect(deployChip(service, 0, [deployment("queued", ["web"])])).toEqual({ kind: "queued" });
    expect(deployChip(service, 0, [deployment("running", ["api"])])).toBeNull();
  });

  it("says what the next Deploy does otherwise", () => {
    expect(deployChip({ ...service, change: "create" }, 0, [])).toEqual({ kind: "staged", label: "New", variant: "success" });
    expect(deployChip({ ...service, change: "update" }, 1, [])).toEqual({ kind: "staged", label: "1 change", variant: "info" });
    expect(deployChip({ ...service, change: "update" }, 3, [])).toEqual({ kind: "staged", label: "3 changes", variant: "info" });
    expect(deployChip({ ...service, change: "update" }, 0, [])).toEqual({ kind: "staged", label: "Changed", variant: "info" });
    expect(deployChip({ ...service, change: "delete" }, 0, [])).toEqual({ kind: "staged", label: "Removing", variant: "destructive" });
  });

  it("shows Deploying only on a Service the running Deploy restarts, once its plan is known", () => {
    const running = [deployment("running", [])];
    expect(deployChip(service, 0, running, new Set(["web"]))).toEqual({ kind: "deploying", since: 100 });
    expect(deployChip(service, 0, running, new Set(["api"]))).toBeNull();
    expect(deployChip({ ...service, change: "update" }, 1, running, new Set())).toEqual({ kind: "staged", label: "1 change", variant: "info" });
    expect(deployChip(service, 0, [...running, deployment("queued", ["web"])], new Set())).toEqual({ kind: "queued" });
    expect(deployChip(service, 0, running, null)).toEqual({ kind: "deploying", since: 100 });
  });
});

describe("restartedServices", () => {
  const node = (type: string, name: string, outcome: string) => asTestDouble<NodeOutcome>()({ type, id: name, name, outcome, rows: [] });

  it("names the Service nodes the Deploy starts or stops, not the unchanged ones or other kinds", () => {
    const nodes = [node("service", "web", "pending"), node("service", "api", "unchanged"), node("service", "worker", "deployed"), node("volume", "data", "pending")];
    expect([...restartedServices({ nodes })]).toEqual(["web", "worker"]);
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

describe("notSetLabel", () => {
  it("counts every secret the Service has no value for", () => {
    expect(notSetLabel(["STRIPE_KEY", "WEBHOOK_SECRET"])).toBe("2 not set");
  });

  it("says nothing when every secret has a value", () => {
    expect(notSetLabel([])).toBeUndefined();
  });
});
