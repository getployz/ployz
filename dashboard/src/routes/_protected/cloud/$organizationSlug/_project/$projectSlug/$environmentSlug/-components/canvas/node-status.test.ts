import type { BuildView, DeploymentView, DeployPreview, DomainRow, NodeOutcome, RowId, ServiceListing } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import type { RuntimeContainerRecord } from "#/modules/runtime/runtime.collection";
import { asTestDouble } from "#/lib/test-double";
import { chipWord, containerTrouble, deployChip, issuesLabel, nodeLabel, deployEffect, nodeIssues, notSetLabel, publicDomain, runningDeployment, runtimeLine, stagedChip, stagedSurface, statusWord, type DeployEffect, type RuntimeLens } from "./node-status";

const service: ServiceListing = { source: "image", change: null, template: null, id: "web", row: "web:node" as RowId, name: "web", private_dns: "web" };
const container = (state: string, health?: string): RuntimeContainerRecord =>
  ({ id: state, displayName: "web", machineId: "m", namespace: "n", kind: "service", runtime: health ? { state, health } : { state } });
const runtime = (...containers: RuntimeContainerRecord[]) => ({ containers });
const observed: RuntimeLens = { status: "observed", incomplete: false, observedAt: "2026-09-30T10:00:00Z", noServers: false, offline: new Set() };
const seen = { lens: observed, desiredReplicas: null, deploy: "leaves" as DeployEffect };
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

  it("says Stopped for a run its Server stopped on request whatever its exit code, and Crashed beside any other failure exit", () => {
    const stopped = { ...container("exited"), runtime: { state: "exited", code: 1, stopped: true as const } };
    const crashed = { ...container("exited"), runtime: { state: "exited", code: 1 } };
    expect(runtimeLine(service, runtime(stopped), seen)).toMatchObject({ word: "Stopped", tone: "bad", down: true });
    expect(runtimeLine(service, runtime(stopped, crashed), seen)).toMatchObject({ word: "Crashed", tone: "crashed", down: true });
  });

  it("says Not running once the Servers' whole evidence has none of it, and Deployed while a Server is missing from it", () => {
    expect(runtimeLine(service, null, seen)).toMatchObject({ word: "Not running", down: true });
    expect(runtimeLine(service, null, watch({ incomplete: true }))).toMatchObject({ word: "Deployed", down: false });
  });

  it("says a grey Not seen running, not Crashed or Stopped, when none of its containers runs while the evidence is incomplete", () => {
    const partial = watch({ incomplete: true });
    const notSeen = { word: "Not seen running", tone: "quiet", down: false };
    expect(runtimeLine(service, runtime(container("exited"), container("restarting")), partial)).toMatchObject(notSeen);
    expect(runtimeLine(service, runtime({ ...container("exited"), runtime: { state: "exited", code: 0 } }), partial)).toMatchObject(notSeen);
    expect(nodeIssues(runtimeLine(service, runtime(container("restarting")), partial), [])).toBeNull();
  });

  it("says Deployed, as with no record, for a record with no container while the evidence is incomplete, leftover hook or not", () => {
    const partial = watch({ incomplete: true });
    const leftoverHook = { containers: [], hookContainers: [{ ...container("exited"), kind: "hook" }] };
    expect(runtimeLine(service, runtime(), partial)).toMatchObject({ word: "Deployed", tone: "quiet", down: false });
    expect(runtimeLine(service, leftoverHook, partial)).toMatchObject({ word: "Deployed", tone: "quiet", down: false });
    expect(nodeIssues(runtimeLine(service, leftoverHook, partial), [])).toBeNull();
    expect(runtimeLine(service, leftoverHook, seen)).toMatchObject({ word: "Not running", tone: "bad", down: true });
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

  it("says a grey Starting while a Deploy starts a Service none of whose containers runs yet, but keeps any down record", () => {
    const deploying = { ...seen, deploy: "restarts" as const };
    const exited = (code: number) => ({ ...container("exited"), runtime: { state: "exited", code } });
    expect(runtimeLine(service, null, deploying)).toMatchObject({ word: "Starting", tone: "quiet", down: false });
    expect(runtimeLine({ ...service, change: "create" }, null, deploying)).toMatchObject({ word: "Starting", tone: "quiet" });
    expect(runtimeLine({ ...service, change: "delete" }, null, deploying)).toMatchObject({ word: "Not running", down: true });
    expect(runtimeLine(service, null, { ...deploying, lens: { ...observed, status: "unavailable" } })).toMatchObject({ word: "Not running", tone: "quiet" });
    expect(runtimeLine(service, runtime(), deploying)).toMatchObject({ word: "Starting", tone: "quiet", down: false });
    expect(nodeIssues(runtimeLine(service, runtime(), deploying), [])).toBeNull();
    expect(runtimeLine({ ...service, change: "delete" }, runtime(), deploying)).toMatchObject({ word: "Not running", down: true });
    expect(runtimeLine(service, runtime(exited(0)), deploying)).toMatchObject({ word: "Stopped", tone: "bad", down: true });
    expect(runtimeLine(service, runtime(exited(1)), deploying)).toMatchObject({ word: "Crashed", tone: "crashed", down: true });
    expect(runtimeLine(service, runtime(container("restarting")), deploying)).toMatchObject({ word: "Crashed", down: true });
  });

  it("says a grey Starting while a first Deploy runs its pre-deploy command, and Not running for a leftover one outside a Deploy", () => {
    const hook: RuntimeContainerRecord = { ...container("running"), kind: "hook" };
    const preDeploy = { containers: [], hookContainers: [hook] };
    expect(runtimeLine({ ...service, change: "create" }, preDeploy, { ...seen, deploy: "restarts" as const })).toMatchObject({ word: "Starting", tone: "quiet", down: false });
    expect(runtimeLine(service, preDeploy, seen)).toMatchObject({ word: "Not running", tone: "bad", down: true });
  });

  describe("while the running Deploy's effect on it is undecided", () => {
    const undecided = { ...seen, deploy: "undecided" as const };
    const hook: RuntimeContainerRecord = { ...container("running"), kind: "hook" };
    const exited = (code: number) => ({ ...container("exited"), runtime: { state: "exited", code } });
    const empty = { "no record": null, "an empty record": runtime(), "a running pre-deploy hook": { containers: [], hookContainers: [hook] } };
    const cases = ([null, "create", "update"] as const).flatMap((change) => Object.entries(empty).map(([record, evidence]) => ({ change, record, evidence })));

    it.each(cases)("is never down for a Service with change $change and $record", ({ change, evidence }) => {
      const status = runtimeLine({ ...service, change }, evidence, undecided);
      expect(status.down).toBe(false);
      expect(nodeIssues(status, [])).toBeNull();
      expect(status).toMatchObject(change === "create" && evidence === null ? { word: "Not deployed", tone: "idle" } : { word: "Checking", tone: "pending" });
    });

    it("keeps the evidence of a container it has, a Server it lost, the Watch's own state and a removal", () => {
      const on = (machineId: string) => ({ ...container("running", "healthy"), id: machineId, machineId });
      expect(runtimeLine(service, runtime(exited(1)), undecided)).toMatchObject({ word: "Crashed", tone: "crashed", down: true });
      expect(runtimeLine(service, runtime(exited(0)), undecided)).toMatchObject({ word: "Stopped", tone: "bad", down: true });
      expect(runtimeLine(service, runtime(on("m2")), { ...undecided, lens: { ...observed, offline: new Set(["m2"]) } })).toMatchObject({ word: "Server offline", down: true });
      expect(runtimeLine(service, runtime(container("running", "healthy")), undecided)).toMatchObject({ word: "Online", tone: "ok" });
      expect(runtimeLine(service, runtime(container("running", "healthy"), container("exited")), { ...undecided, desiredReplicas: 2 }).word).toBe("Degraded");
      expect(runtimeLine(service, null, { ...undecided, lens: { ...observed, status: "unavailable" } })).toMatchObject({ word: "Not running", tone: "quiet", down: false });
      expect(runtimeLine(service, null, { ...undecided, lens: { ...observed, incomplete: true } })).toMatchObject({ word: "Deployed", tone: "quiet" });
      expect(runtimeLine({ ...service, change: "delete" }, null, undecided)).toMatchObject({ word: "Not running", tone: "bad", down: true });
      expect(runtimeLine({ ...service, change: "delete" }, runtime(), undecided)).toMatchObject({ word: "Not running", down: true });
    });

    it("trades a red Not running for Checking on a Service that's really down, while a Deploy that ships it but may not start it has no plan", () => {
      const reallyDown = runtime();
      expect(runtimeLine(service, reallyDown, undecided)).toMatchObject({ word: "Checking", down: false });
      expect(runtimeLine(service, reallyDown, { ...seen, deploy: "leaves" })).toMatchObject({ word: "Not running", tone: "bad", down: true });
    });
  });

  it("says a grey Status unavailable, not a shimmer or Not running, when the running Deploy's view is refused or fails to read", () => {
    const unreadable = { ...seen, deploy: "unreadable" as const };
    expect(runtimeLine(service, null, unreadable)).toMatchObject({ word: "Status unavailable", tone: "quiet", down: false });
    expect(runtimeLine({ ...service, change: "update" }, runtime(), unreadable)).toMatchObject({ word: "Status unavailable", tone: "quiet", down: false });
    expect(nodeIssues(runtimeLine(service, null, unreadable), [])).toBeNull();
    expect(runtimeLine({ ...service, change: "delete" }, null, unreadable)).toMatchObject({ word: "Not running", down: true });
    expect(runtimeLine(service, runtime(container("restarting")), unreadable)).toMatchObject({ word: "Crashed", down: true });
  });

  it("says Not deployed for a new Service with no record until a running Deploy starts it", () => {
    const create = { ...service, change: "create" as const };
    expect((["leaves", "undecided", "unreadable"] as const).map((deploy) => runtimeLine(create, null, { ...seen, deploy }).word)).toEqual(["Not deployed", "Not deployed", "Not deployed"]);
    expect(runtimeLine(create, null, { ...seen, deploy: "restarts" }).word).toBe("Starting");
  });

  it("says Degraded when fewer replicas serve than it asks for, except while a Deploy rolls them or a Server didn't report", () => {
    const one = runtime(container("running", "healthy"), container("exited"));
    expect(runtimeLine(service, one, { ...seen, desiredReplicas: 2 }).word).toBe("Degraded");
    expect(runtimeLine(service, one, { ...seen, desiredReplicas: 2, deploy: "restarts" as const }).word).toBe("Online");
    expect(runtimeLine(service, one, { ...seen, desiredReplicas: 1 }).word).toBe("Online");
    expect(runtimeLine(service, one, { ...watch({ incomplete: true }), desiredReplicas: 2 }).word).toBe("Online");
  });
  it("reads containers an offline Server last reported as gone: Server offline when they were all it had, Degraded when others serve", () => {
    const on = (machineId: string) => ({ ...container("running", "healthy"), id: machineId, machineId });
    const offline = watch({ offline: new Set(["m2"]) });
    expect(runtimeLine(service, runtime(on("m2")), offline)).toMatchObject({ word: "Server offline", tone: "bad", down: true });
    expect(runtimeLine(service, runtime(on("m1"), on("m2")), { ...offline, desiredReplicas: 2 })).toMatchObject({ word: "Degraded", tone: "warn" });
    expect(runtimeLine(service, runtime(on("m1"), on("m2")), { ...seen, desiredReplicas: 2 }).word).toBe("Online");
    expect(runtimeLine(service, runtime(on("m2")), { ...offline, lens: { ...offline.lens, status: "unavailable" } })).toMatchObject({ word: "Server offline", tone: "quiet", down: false });
    expect(runtimeLine(service, runtime(on("m2")), watch({ offline: new Set(["m2"]), incomplete: true }))).toMatchObject({ word: "Server offline", tone: "quiet", down: false });
  });
});

describe("nodeIssues", () => {
  const domain = (status: DomainRow["status"], hostname: string | null = "acme.com") => ({ status, hostname });

  it("counts what the user can fix, red when a Service is down", () => {
    expect(nodeIssues(runtimeLine(service, runtime(container("exited")), seen), [domain("needs_attention")])).toEqual({ count: 2, tone: "bad", monitoring: false, lead: null });
    expect(nodeIssues(runtimeLine(service, runtime(), seen), [])).toEqual({ count: 1, tone: "bad", monitoring: false, lead: null });
    expect(nodeIssues(runtimeLine(service, runtime(container("running", "healthy")), seen), [domain("needs_attention"), domain("setting_up")]))
      .toEqual({ count: 1, tone: "warn", monitoring: false, lead: null });
  });

  it("counts one amber warning for a failing healthcheck, however many replicas fail it", () => {
    const failing = runtime(container("running", "failing"), container("running", "failing"));
    expect(nodeIssues(runtimeLine(service, failing, seen), [])).toEqual({ count: 1, tone: "warn", monitoring: false, lead: "Check failing" });
    expect(nodeIssues(runtimeLine(service, failing, seen), [domain("needs_attention")])).toEqual({ count: 2, tone: "warn", monitoring: false, lead: "Check failing" });
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

    it("adds one red issue named for it", () => {
      expect(nodeIssues(healthy, [], { unreachable: "acme.com" })).toEqual({ count: 1, tone: "bad", monitoring: true, lead: "Unreachable" });
      expect(nodeIssues(runtimeLine(service, runtime(container("running", "failing")), seen), [], { unreachable: "acme.com" })).toEqual({ count: 2, tone: "bad", monitoring: true, lead: "Unreachable" });
    });

    it("adds nothing when the Service is already down", () => {
      expect(nodeIssues(runtimeLine(service, runtime(), seen), [], { unreachable: "acme.com" })).toEqual({ count: 1, tone: "bad", monitoring: false, lead: null });
    });

    it("shares one issue with a domain needing attention on the same hostname", () => {
      expect(nodeIssues(healthy, [domain("needs_attention")], { unreachable: "acme.com" })).toEqual({ count: 1, tone: "bad", monitoring: false, lead: "Unreachable" });
      expect(nodeIssues(healthy, [domain("needs_attention", "web.acme.ployz.app")], { unreachable: "acme.com" })).toEqual({ count: 2, tone: "bad", monitoring: true, lead: "Unreachable" });
    });
  });

  describe("with an OOM kill or restart in the last hour", () => {
    const healthy = runtimeLine(service, runtime(container("running", "healthy")), seen);

    it("adds one amber issue named for it, which opens Monitoring", () => {
      expect(nodeIssues(healthy, [], { trouble: "OOM" })).toEqual({ count: 1, tone: "warn", monitoring: true, lead: "OOM" });
    });

    it("counts beside a Service that's down, which stays red", () => {
      expect(nodeIssues(runtimeLine(service, runtime(container("exited")), seen), [], { trouble: "Restarting" }))
        .toEqual({ count: 2, tone: "bad", monitoring: true, lead: "Restarting" });
    });
  });
});

describe("containerTrouble", () => {
  const web1 = { id: "m1", name: "web-1" };
  const web2 = { id: "m2", name: "web-2" };
  const now = Date.UTC(2026, 9, 12, 12);
  const marker = (kind: "oom" | "exit" | "restart", replica: string, server = web1, at = now - 60_000) => ({ kind, replica, server, at });

  const crashed = (replica: string, server = web1, at = now - 60_000) => [
    { ...marker("exit", replica, server, at - 2_000), exitCode: 1 },
    marker("restart", replica, server, at),
  ];

  it("says nothing when each replica crashed and came back once", () => {
    expect(containerTrouble([...crashed("web-1"), ...crashed("web-2"), ...crashed("web-3")], now)).toBeNull();
  });

  it("says Restarting when one replica crashed and came back twice", () => {
    expect(containerTrouble([...crashed("web-1", web1, now - 5 * 60_000), ...crashed("web-2"), ...crashed("web-1")], now)).toBe("Restarting");
  });

  it("counts a restart after an OOM kill or an exit with no code only for what they are", () => {
    const afterOom = [marker("oom", "web-1", web1, now - 3 * 60_000), marker("restart", "web-1", web1, now - 2 * 60_000)];
    expect(containerTrouble(afterOom, now)).toBe("OOM");
    const unknownExit = [10, 5].flatMap((ago) => [marker("exit", "web-1", web1, now - ago * 60_000), marker("restart", "web-1", web1, now - ago * 60_000 + 1_000)]);
    expect(containerTrouble(unknownExit, now)).toBeNull();
  });

  it("needs a crash before each restart it counts", () => {
    const once = [...crashed("web-1", web1, now - 5 * 60_000), marker("restart", "web-1", web1, now - 2 * 60_000)];
    expect(containerTrouble(once, now)).toBeNull();
  });

  it("forgets a crash loop once the replica exits cleanly", () => {
    const loop = [...crashed("web-1", web1, now - 8 * 60_000), ...crashed("web-1", web1, now - 6 * 60_000)];
    const deploy = { ...marker("exit", "web-1", web1, now - 4 * 60_000), exitCode: 0 };
    expect(containerTrouble([...loop, deploy], now)).toBeNull();
    expect(containerTrouble([...loop, deploy, ...crashed("web-1", web1, now - 2 * 60_000)], now)).toBeNull();
  });

  it("takes an exit by a signal only a stop sends for a requested stop, and a fault signal for a crash", () => {
    const exits = (code: number) =>
      [8, 6].flatMap((ago) => [{ ...marker("exit", "web-1", web1, now - ago * 60_000), exitCode: code }, marker("restart", "web-1", web1, now - ago * 60_000 + 1_000)]);
    for (const stop of [129, 130, 131, 137, 138, 140, 143]) expect(containerTrouble(exits(stop), now)).toBeNull();
    for (const fault of [2, 132, 134, 135, 136, 139]) expect(containerTrouble(exits(fault), now)).toBe("Restarting");
  });

  it("starts a new loop when a crash comes after ten quiet minutes", () => {
    expect(containerTrouble([...crashed("web-1", web1, now - 50 * 60_000), ...crashed("web-1", web1, now - 5 * 60_000)], now)).toBeNull();
    expect(containerTrouble([...crashed("web-1", web1, now - 14 * 60_000), ...crashed("web-1", web1, now - 5 * 60_000)], now)).toBe("Restarting");
  });

  it("tells apart same-named replicas on different Servers", () => {
    expect(containerTrouble([...crashed("web-1", web1), ...crashed("web-1", web2)], now)).toBeNull();
  });

  it("says OOM for one OOM kill, over any restarts, and nothing for exits alone", () => {
    expect(containerTrouble([marker("oom", "web-1")], now)).toBe("OOM");
    expect(containerTrouble([...crashed("web-1", web1, now - 5 * 60_000), ...crashed("web-1"), marker("oom", "web-2")], now)).toBe("OOM");
    expect(containerTrouble([marker("exit", "web-1"), marker("exit", "web-1")], now)).toBeNull();
    expect(containerTrouble([], now)).toBeNull();
  });

  it("says nothing for Deploys that stop a replica cleanly and start it again", () => {
    const minute = 60_000;
    const exited = (code: number, at: number) => ({ ...marker("exit", "nginx-1", web1, at), exitCode: code });
    const deploys = [40, 30, 20, 10].flatMap((ago) => [exited(0, now - ago * minute), exited(0, now - ago * minute + 1_000)]);
    const restarts = [30, 20, 10].map((ago) => marker("restart", "nginx-1", web1, now - ago * minute + 3_000));
    expect(containerTrouble([...deploys, ...restarts].sort((a, b) => a.at - b.at), now)).toBeNull();
  });

  it("drops Restarting once a crash loop goes quiet for ten minutes", () => {
    const minute = 60_000;
    const crashLoop = [25, 20, 15].flatMap((ago) => [
      { ...marker("exit", "web-1", web1, now - ago * minute), exitCode: 1 },
      marker("restart", "web-1", web1, now - ago * minute + 2_000),
    ]);
    expect(containerTrouble(crashLoop, now - 6 * minute)).toBe("Restarting");
    expect(containerTrouble(crashLoop, now - 4 * minute)).toBeNull();
  });

  it("counts only markers from the hour before now", () => {
    const hour = 60 * 60_000;
    expect(containerTrouble([marker("oom", "web-1", web1, now - hour + 1)], now)).toBe("OOM");
    expect(containerTrouble([marker("oom", "web-1", web1, now - hour)], now)).toBeNull();
    expect(containerTrouble([...crashed("web-1", web1, now - 2 * hour), ...crashed("web-1")], now)).toBeNull();
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
  const deployment = (status: "queued" | "running", services: string[], ships = services) => ({ status, services, ships, started_at: status === "running" ? 100 : null, admitted_at: 90 });

  it("puts a Deploy in flight first: running, else queued, when it targets the Service or every Service", () => {
    expect(deployChip({ ...service, change: "update" }, 2, [deployment("queued", []), deployment("running", ["web"])], "restarts")).toEqual({ kind: "deploying", since: 100 });
    expect(deployChip(service, 0, [deployment("queued", ["web"])])).toEqual({ kind: "queued" });
    expect(deployChip(service, 0, [deployment("running", ["api"])], "leaves")).toBeNull();
  });

  it("queues a Service a Deploy ships because a Service it names references it", () => {
    expect(deployChip(service, 0, [deployment("queued", ["api"], ["api", "web"])])).toEqual({ kind: "queued" });
  });

  it("says what the next Deploy does otherwise", () => {
    expect(deployChip({ ...service, change: "create" }, 0, [])).toEqual({ kind: "staged", label: "New", variant: "changed" });
    expect(deployChip({ ...service, change: "update" }, 1, [])).toEqual({ kind: "staged", label: "1 change", variant: "changed" });
    expect(deployChip({ ...service, change: "update" }, 3, [])).toEqual({ kind: "staged", label: "3 changes", variant: "changed" });
    expect(deployChip({ ...service, change: "update" }, 0, [])).toEqual({ kind: "staged", label: "Changed", variant: "changed" });
    expect(deployChip({ ...service, change: "delete" }, 0, [])).toEqual({ kind: "staged", label: "Removing", variant: "destructive" });
  });

  it("shows Deploying only on a Service the running Deploy restarts, once its plan is known", () => {
    const running = [deployment("running", [])];
    expect(deployChip(service, 0, running, "restarts")).toEqual({ kind: "deploying", since: 100 });
    expect(deployChip(service, 0, running, "leaves")).toBeNull();
    expect(deployChip({ ...service, change: "update" }, 1, running, "leaves")).toEqual({ kind: "staged", label: "1 change", variant: "changed" });
    expect(deployChip(service, 0, [...running, deployment("queued", ["web"])], "leaves")).toEqual({ kind: "queued" });
  });

  it("waits for the running Deploy's plan before it shows Deploying, and never shows it when the view can't be read", () => {
    const running = [deployment("running", [])];
    expect(deployChip(service, 0, running, "undecided")).toBeNull();
    expect(deployChip({ ...service, change: "update" }, 1, running, "undecided")).toEqual({ kind: "staged", label: "1 change", variant: "changed" });
    expect(deployChip(service, 0, [...running, deployment("queued", ["web"])], "undecided")).toEqual({ kind: "queued" });
    expect(deployChip(service, 0, running, "unreadable")).toBeNull();
  });

  it("never takes Deploying off a card to bring it back, from the running summary through its view, builds and plan", () => {
    const running = [deployment("running", [])];
    const node = (outcome: string) => asTestDouble<NodeOutcome>()({ type: "service", id: "web", name: "web", outcome, rows: [] });
    const build = (status: string) => asTestDouble<BuildView>()({ service: "web", status });
    const planned = asTestDouble<DeployPreview>()({ operations: [] });
    const chip = (view: Pick<DeploymentView, "nodes" | "preview" | "builds"> | null) =>
      deployChip({ ...service, change: "update" }, 1, running, deployEffect("web", running[0] ?? null, { result: view === null ? undefined : { ok: true, value: view }, failed: false }))?.kind === "deploying";
    const built = [null, { nodes: [node("pending")], preview: null, builds: [build("building")] },
      { nodes: [node("pending")], preview: null, builds: [build("built")] }, { nodes: [node("pending")], preview: planned, builds: [build("built")] }];
    expect(built.map(chip)).toEqual([false, false, true, true]);
    const configOnly = [null, { nodes: [node("pending")], preview: null, builds: [] }, { nodes: [node("pending")], preview: planned, builds: [] }];
    expect(configOnly.map(chip)).toEqual([false, false, true]);
  });

  it("shows Deploying on a Service a narrowed Deploy restarts though it didn't select it", () => {
    const narrowed = deployment("running", ["client"], ["client", "web"]);
    const node = (name: string) => asTestDouble<NodeOutcome>()({ type: "service", id: name, name, outcome: "pending", rows: [] });
    const view = { ok: true as const, value: { nodes: [node("client"), node("web")], preview: asTestDouble<DeployPreview>()({ operations: [] }), builds: [] } };
    expect(deployChip(service, 0, [narrowed], deployEffect("web", narrowed, { result: view, failed: false }))).toEqual({ kind: "deploying", since: 100 });
  });
});

describe("deployEffect", () => {
  const node = (type: string, name: string, outcome: string) => asTestDouble<NodeOutcome>()({ type, id: name, name, outcome, rows: [] });
  const build = (service: string, status: string) => asTestDouble<BuildView>()({ service, status });
  const planned = asTestDouble<DeployPreview>()({ operations: [] });
  const every = { services: [], ships: [] };
  const read = (view: Pick<DeploymentView, "nodes" | "preview" | "builds">, failed = false) => ({ result: { ok: true as const, value: view }, failed });
  const unread = { result: undefined, failed: false };

  it("restarts the Service nodes the plan starts or stops, and leaves the unchanged ones, other kinds and those it doesn't name", () => {
    const view = read({ nodes: [node("service", "web", "pending"), node("service", "api", "unchanged"), node("service", "worker", "deployed"), node("volume", "data", "pending")], preview: planned, builds: [] });
    expect(["web", "api", "worker", "data", "redis"].map((name) => deployEffect(name, every, view))).toEqual(["restarts", "leaves", "restarts", "leaves", "leaves"]);
  });

  it("restarts only a Service it built a new image for until its plan is recorded; any other is undecided, a reused image included", () => {
    const nodes = ["api", "web", "static", "dockerfile"].map((name) => node("service", name, "pending"));
    const builds = [build("api", "built"), build("static", "reused"), build("dockerfile", "building")];
    expect(["api", "web", "static", "dockerfile"].map((name) => deployEffect(name, every, read({ nodes, preview: null, builds }))))
      .toEqual(["restarts", "undecided", "undecided", "undecided"]);
  });

  it("leaves every Service while no Deploy runs, a queued one included: it starts nothing yet", () => {
    const queued = [{ status: "queued" as const }];
    expect(runningDeployment(queued)).toBeNull();
    expect(deployEffect("web", runningDeployment([{ ...every, status: "queued" as const }]), unread)).toBe("leaves");
    expect(runtimeLine(service, runtime(), { ...seen, deploy: deployEffect("web", null, unread) })).toMatchObject({ word: "Not running", tone: "bad", down: true });
    expect(runningDeployment([...queued, { status: "running" as const }])).toEqual({ status: "running" });
  });

  it("leaves a Service a narrowed Deploy doesn't ship, before its view is read", () => {
    const narrowed = { services: ["api"], ships: ["api", "postgres"] };
    expect(["web", "api", "postgres"].map((name) => deployEffect(name, narrowed, unread))).toEqual(["leaves", "undecided", "undecided"]);
  });

  it("is undecided until its view is read, and unreadable once the view is refused or its read fails", () => {
    expect(deployEffect("web", every, unread)).toBe("undecided");
    expect(deployEffect("web", every, { result: undefined, failed: true })).toBe("unreadable");
    expect(deployEffect("web", every, { result: { ok: false, refusal: { code: "not_found", message: "No such Deployment", details: null } }, failed: false })).toBe("unreadable");
  });

  it("turns undecided into unreadable when a later read fails, and keeps what a recorded plan or built image already says", () => {
    const pending = [node("service", "web", "pending"), node("service", "api", "pending")];
    expect(deployEffect("web", every, read({ nodes: pending, preview: null, builds: [] }, true))).toBe("unreadable");
    expect(deployEffect("api", every, read({ nodes: pending, preview: null, builds: [build("api", "built")] }, true))).toBe("restarts");
    expect(deployEffect("web", every, read({ nodes: pending, preview: planned, builds: [] }, true))).toBe("restarts");
  });

  it("never takes a Service off and back on as its build and then its plan come in", () => {
    const nodes = (outcome: (name: string) => string) => ["api", "web", "static"].map((name) => node("service", name, outcome(name)));
    const pending = nodes(() => "pending");
    const steps = [
      unread,
      read({ nodes: pending, preview: null, builds: [build("api", "pending"), build("web", "pending"), build("static", "pending")] }),
      read({ nodes: pending, preview: null, builds: [build("api", "building"), build("web", "building"), build("static", "building")] }),
      read({ nodes: pending, preview: null, builds: [build("api", "building"), build("web", "reused"), build("static", "reused")] }),
      read({ nodes: pending, preview: null, builds: [build("api", "built"), build("web", "reused"), build("static", "reused")] }),
      read({ nodes: nodes((name) => name === "static" ? "unchanged" : "pending"), preview: planned, builds: [] }),
    ];
    const shown = (name: string) => steps.map((step) => deployEffect(name, every, step));
    expect(shown("api")).toEqual(["undecided", "undecided", "undecided", "undecided", "restarts", "restarts"]);
    expect(shown("web")).toEqual(["undecided", "undecided", "undecided", "undecided", "undecided", "restarts"]);
    expect(shown("static")).toEqual(["undecided", "undecided", "undecided", "undecided", "undecided", "leaves"]);
  });
});

describe("stagedSurface", () => {
  it("is green for a new node, blue for a change and red for a removal", () => {
    expect(stagedSurface("create")).toBe("changed");
    expect(stagedSurface("update")).toBe("changed");
    expect(stagedSurface("delete")).toBe("destructive");
    expect(stagedSurface(null)).toBeUndefined();
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

describe("nodeLabel", () => {
  it("reads a node's name, then its card's words, skipping what it doesn't show", () => {
    const issues = { count: 1, tone: "bad", monitoring: true, lead: "Unreachable" } as const;
    expect(nodeLabel("api", chipWord({ kind: "queued" }), chipWord(stagedChip("update", 2)), "Can't reach servers", issuesLabel(issues), undefined))
      .toBe("api, Queued, 2 changes, Can't reach servers, Unreachable, 1 issue");
    expect(nodeLabel("redis", chipWord(null), null)).toBe("redis");
    expect(chipWord({ kind: "deploying", since: 0 })).toBe("Deploying");
    expect(issuesLabel({ count: 2, tone: "warn", monitoring: false, lead: null })).toBe("2 issues");
  });

  it("reads the shimmer as Checking", () => {
    expect(statusWord({ word: "", tone: "pending", since: null })).toBe("Checking");
    expect(statusWord({ word: "Online", tone: "ok", since: null })).toBe("Online");
  });

  it("reads a grey word's age, as its line shows it", () => {
    const now = new Date("2026-10-29T12:00:00Z");
    expect(statusWord({ word: "Online", tone: "quiet", since: new Date("2026-10-29T09:00:00Z") }, now)).toBe("Online 3 hours ago");
  });
});
