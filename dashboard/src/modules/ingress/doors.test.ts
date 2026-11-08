import { Effect } from "effect";
import { describe, expect, it } from "vitest";
import type { RuntimeWatchView } from "@ployz/sdk";
import { doorsOpenOn } from "#/modules/cluster-domain/ingress-running";
import { type CloudflareSetup, doorsGap, doorWords, type LatestDoorsChange, movedHostnames, redeployScope, switchProblem, switchProgress } from "#/modules/ingress/doors";
import { runSwitch, switchSteps, type SwitchPorts } from "#/modules/ingress/doors.run";
import { asTestDouble } from "#/lib/test-double";

const answer = (...perServer: ReadonlyArray<readonly string[]>) => ({
  machines: perServer.map((doors, index) => ({ server: `s${index + 1}`, doors })),
  failures: [],
});

/** Ports that record what the switch did, with the Doors each Server holds after every set, and why a redeploy stuck. */
function ports(held: (doors: readonly string[]) => ReturnType<typeof answer>, log: string[], stuck: string | null = null): SwitchPorts<never, never, never> {
  return {
    open: (doors) => Effect.sync(() => { log.push(`open ${doors.join("+")}`); return held(doors); }),
    at: (stage) => Effect.sync(() => { log.push(`at ${stage}`); }),
    flip: () => Effect.sync(() => { log.push("flip"); }),
    redeploy: () => Effect.sync(() => { log.push("redeploy"); return stuck; }),
  };
}

describe("the two-step door switch", () => {
  it("opens both Doors, moves the names, then closes the old Door", async () => {
    const log: string[] = [];
    const end = await Effect.runPromise(runSwitch("tunnel", ports((doors) => answer(doors, doors), log)));
    expect(end).toEqual({ ok: true });
    expect(log).toEqual(["at opening", "open public+tunnel", "at moving", "flip", "redeploy", "at closing", "open tunnel"]);
  });

  it("goes back the same way, ending on public ports", async () => {
    const log: string[] = [];
    await Effect.runPromise(runSwitch("public", ports((doors) => answer(doors), log)));
    expect(log.filter((line) => line.startsWith("open"))).toEqual(["open public+tunnel", "open public"]);
    expect(switchSteps("public")).toEqual([["public", "tunnel"], ["public"]]);
  });

  it("stops before moving anything when a Server doesn't take both Doors", async () => {
    const log: string[] = [];
    const end = await Effect.runPromise(runSwitch("tunnel", ports(() => answer(["public", "tunnel"], ["public"]), log)));
    expect(end).toEqual({ ok: false, failure: "s2 still has other doors open." });
    expect(log).toEqual(["at opening", "open public+tunnel"]);
  });

  it("keeps both Doors open and says why when an Environment doesn't redeploy under the new source", async () => {
    const log: string[] = [];
    const end = await Effect.runPromise(runSwitch("tunnel", ports((doors) => answer(doors), log, "shop/production didn't redeploy: it would delete pg-data.")));
    expect(end).toEqual({ ok: false, failure: "shop/production didn't redeploy: it would delete pg-data." });
    expect(log).toEqual(["at opening", "open public+tunnel", "at moving", "flip", "redeploy"]);
  });

  it("leaves both Doors open when closing the old one fails", async () => {
    const log: string[] = [];
    const end = await Effect.runPromise(runSwitch("tunnel", ports((doors) => doors.length === 2 ? answer(doors) : answer(["public", "tunnel"]), log)));
    expect(end).toMatchObject({ ok: false });
    expect(log.at(-1)).toBe("open tunnel");
  });

  it("reports a Server that refused, by its own words", () => {
    expect(doorsGap({ machines: [], failures: [{ error: { message: "no connector" } }] }, ["tunnel"])).toBe("A server didn't switch: no connector");
    expect(doorsGap(answer(["tunnel"]), ["tunnel"])).toBeNull();
    expect(doorsGap(answer(), ["tunnel"])).toBe("No server takes web traffic yet.");
  });
});

describe("what a switch moves", () => {
  const published = [
    { hostname: "web-prod.acme.ployz.app", namespace: "shop-prod", service: "web" },
    { hostname: "deep.web-prod.acme.ployz.app", namespace: "shop-prod", service: "deep" },
    { hostname: "www.example.org", namespace: "shop-prod", service: "web" },
  ];

  it("moves each generated hostname one label below the new source, and leaves custom domains alone", () => {
    expect(movedHostnames(published, "acme.ployz.app", "example.com")).toEqual([
      { from: "web-prod.acme.ployz.app", to: "web-prod.example.com", namespace: "shop-prod", service: "web" },
    ]);
  });

  it("redeploys only the deployed Services publishing a moved hostname, by their Store names, and never a whole Environment", () => {
    const moving = [
      { from: "web-prod.acme.ployz.app", to: "web-prod.example.com", namespace: "shop-prod", service: "web-1" },
      { from: "api-prod.acme.ployz.app", to: "api-prod.example.com", namespace: "shop-prod", service: "api" },
      { from: "docs.acme.ployz.app", to: "docs.example.com", namespace: "docs-main", service: "site" },
    ];
    const owners = [
      { namespace: "shop-prod", project: "shop", environment: "production" },
      { namespace: "docs-main", project: "docs", environment: "main" },
    ];
    const runtimeNames = new Map<string, Record<string, string>>([
      ["shop-prod", { web: "web-1", api: "api", worker: "worker", postgres: "postgres" }],
      ["docs-main", { site: "site" }],
    ]);
    expect(redeployScope(moving, owners, runtimeNames)).toEqual({
      environments: [
        { project: "shop", environment: "production", services: ["api", "web"] },
        { project: "docs", environment: "main", services: ["site"] },
      ],
      left: [],
    });
  });

  it("leaves over a moved hostname whose Service the Store can't name, instead of deploying around it", () => {
    const moved = { from: "old.acme.ployz.app", to: "old.example.com", namespace: "gone", service: "web" };
    expect(redeployScope([moved], [], new Map())).toEqual({ environments: [], left: [moved] });
    const owner = { namespace: "gone", project: "shop", environment: "old" };
    expect(redeployScope([moved], [owner], new Map([["gone", { api: "api" }]]))).toEqual({ environments: [], left: [moved] });
  });
});

describe("when the Cluster can switch", () => {
  const setup = (state: CloudflareSetup["state"]): CloudflareSetup => ({ zone: "example.com", tunnelName: "ployz-acme", tunnelId: "t1", state });

  it("goes to Cloudflare only once it is set up and not yet in use", () => {
    expect(switchProblem(null, "tunnel")).toBe("Set up Cloudflare first, then switch to it.");
    expect(switchProblem(setup("set_up"), "tunnel")).toBeNull();
    expect(switchProblem(setup("active"), "tunnel")).toBe("This Cluster already uses Cloudflare.");
    expect(switchProblem(setup("left"), "tunnel")).toBe("Set up Cloudflare again, then switch to it.");
  });

  it("goes back to public ports from anywhere but public ports", () => {
    expect(switchProblem(setup("active"), "public")).toBeNull();
    expect(switchProblem(setup("left"), "public")).toBe("This Cluster already uses public ports.");
  });

  const ended = (want: "tunnel" | "public", state: "failed" | "finished"): LatestDoorsChange => state === "failed"
    ? { attemptId: "a", want, state, endedAt: "2026-11-08T00:00:00Z", failure: "s1 still has other doors open." }
    : { attemptId: "a", want, state, endedAt: "2026-11-08T00:00:00Z" };

  it("lets a switch whose source moved but whose last step failed be asked again, and only that one", () => {
    expect(switchProblem(setup("active"), "tunnel", ended("tunnel", "failed"))).toBeNull();
    expect(switchProblem(setup("active"), "tunnel", ended("tunnel", "finished"))).toBe("This Cluster already uses Cloudflare.");
    expect(switchProblem(setup("active"), "tunnel", ended("public", "failed"))).toBe("This Cluster already uses Cloudflare.");
    expect(switchProblem(setup("left"), "public", ended("public", "failed"))).toBeNull();
    expect(switchProblem(setup("left"), "public", ended("tunnel", "failed"))).toBe("This Cluster already uses public ports.");
  });
});

describe("the Doors the Servers hold", () => {
  type Service = RuntimeWatchView["services"][number];
  type Container = Service["containers"][number];
  const container = (machine_id: string, state: Container["runtime"]["state"], ports: number[]) => asTestDouble<Container>()({
    machine_id, runtime: { state }, resolved_spec: { ports: ports.map((published_port) => ({ mode: "host", published_port })) },
  });
  const service = (identity: string, containers: Container[]) => asTestDouble<Service>()({ identity, containers });
  const frame: Pick<RuntimeWatchView, "services"> = {
    services: [
      service("ployz-system/ingress-holder", [container("m1", "running", [80, 443]), container("m2", "running", []), container("m3", "exited", [80, 443])]),
      service("ployz-system/tunnel", [container("m2", "running", []), container("m3", "running", [])]),
      service("shop-prod/web", [container("m1", "running", [80])]),
    ],
  };

  it("reads each Server's Doors from its running door containers: host ports open Public, a connector opens Tunnel", () => {
    const open = doorsOpenOn(frame);
    expect([...open.entries()].map(([machine, doors]) => [machine, [...doors].sort()])).toEqual([["m1", ["public"]], ["m2", ["tunnel"]], ["m3", ["tunnel"]]]);
  });

  it("says of a Door what the Servers show, and nothing before any was seen", () => {
    const seen = (...doors: ReadonlyArray<readonly ("public" | "tunnel")[]>) => ({
      servers: doors.map((held, index) => ({ id: `m${index + 1}`, name: `s${index + 1}`, doors: held })), hostnames: [], partial: false,
    });
    expect(doorWords(null, "public")).toBeNull();
    expect(doorWords(seen(), "public")).toBe("No server takes web traffic yet.");
    expect(doorWords(seen(["public", "tunnel"], ["tunnel"]), "public")).toBe("Open on 1 of 2 servers.");
    expect(doorWords(seen(["public", "tunnel"], ["tunnel"]), "tunnel")).toBe("Open on every server.");
    expect(doorWords(seen(["tunnel"]), "public")).toBe("Closed on every server.");
  });
});

describe("progress", () => {
  it("says where a switch is, and nothing once it ended", () => {
    const base = { attemptId: "a", want: "tunnel" as const };
    expect(switchProgress(null)).toBeNull();
    expect(switchProgress({ ...base, state: "finished", endedAt: "2026-11-08T00:00:00Z" })).toBeNull();
    expect(switchProgress({ ...base, state: "running", startedAt: "2026-11-08T00:00:00Z", stage: "moving" })).toMatchObject({ step: 2 });
    expect(switchProgress({ ...base, state: "running", startedAt: "2026-11-08T00:00:00Z", stage: null })).toMatchObject({ step: 1 });
  });
});
