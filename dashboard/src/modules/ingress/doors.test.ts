import { Effect } from "effect";
import { describe, expect, it } from "vitest";
import type { RuntimeWatchView } from "@ployz/sdk";
import { doorsOpenOn } from "#/modules/cluster-domain/ingress-running";
import {
  type CloudflareSetup,
  collidingMoves,
  collisionGap,
  doorsGap,
  doorWords,
  type LatestDoorsChange,
  keptHostnames,
  type KnownRoutes,
  movedHostnames,
  type MovedHostname,
  observationGap,
  redeployScope,
  routeKey,
  switchProblem,
  switchProgress,
  unknownGap,
  unknownHostnames,
  unservedMoves,
} from "#/modules/ingress/doors";
import { redeployMoved, type RedeployPorts, runSwitch, switchSteps, type SwitchPorts } from "#/modules/ingress/doors.run";
import { asTestDouble } from "#/lib/test-double";

const answer = (...perServer: ReadonlyArray<readonly string[]>) => ({
  machines: perServer.map((doors, index) => ({ server: `s${index + 1}`, doors })),
  failures: [],
});

const MOVE: MovedHostname = { from: "web-prod.acme.ployz.app", to: "web-prod.example.com", namespace: "shop-prod", service: "web" };

/**
 * Ports that record what the switch did, with the Doors each Server holds after every set, what the Servers showed as
 * moving (or why Cloud couldn't tell), and why a redeploy stuck.
 */
function ports(
  held: (doors: readonly string[]) => ReturnType<typeof answer>, log: string[],
  { stuck = null, seen = { moves: [MOVE] } }: { stuck?: string | null; seen?: { moves: readonly MovedHostname[] } | { failure: string } } = {},
): SwitchPorts<never, never, never> {
  return {
    open: (doors) => Effect.sync(() => { log.push(`open ${doors.join("+")}`); return held(doors); }),
    at: (stage) => Effect.sync(() => { log.push(`at ${stage}`); }),
    moving: () => Effect.sync(() => { log.push("moving"); return seen; }),
    flip: () => Effect.sync(() => { log.push("flip"); }),
    redeploy: (moves) => Effect.sync(() => { log.push(`redeploy ${moves.map(({ to }) => to).join(",")}`); return stuck; }),
  };
}

describe("the two-step door switch", () => {
  it("opens both Doors, moves the names, then closes the old Door", async () => {
    const log: string[] = [];
    const end = await Effect.runPromise(runSwitch("tunnel", ports((doors) => answer(doors, doors), log)));
    expect(end).toEqual({ ok: true });
    expect(log).toEqual(["at opening", "open public+tunnel", "at moving", "moving", "flip", "redeploy web-prod.example.com", "at closing", "open tunnel"]);
  });

  it("opens only the tunnel on a Cluster founded closed, so ports 80 and 443 never open on the way", async () => {
    const log: string[] = [];
    const end = await Effect.runPromise(runSwitch("tunnel", ports((doors) => answer(doors, doors), log), true));
    expect(end).toEqual({ ok: true });
    expect(log).toEqual(["at opening", "open tunnel", "at moving", "moving", "flip", "redeploy web-prod.example.com", "at closing", "open tunnel"]);
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
    expect(end).toEqual({ ok: false, failure: "s2 didn't switch." });
    expect(log).toEqual(["at opening", "open public+tunnel"]);
  });

  it("keeps both Doors open and says why when an Environment doesn't redeploy under the new source", async () => {
    const log: string[] = [];
    const end = await Effect.runPromise(runSwitch("tunnel", ports((doors) => answer(doors), log, { stuck: "shop/production didn't redeploy: it would delete pg-data." })));
    expect(end).toEqual({ ok: false, failure: "shop/production didn't redeploy: it would delete pg-data." });
    expect(log).toEqual(["at opening", "open public+tunnel", "at moving", "moving", "flip", "redeploy web-prod.example.com"]);
  });

  it("stops with both Doors open, before the source moves, when a look at the Servers missed some", async () => {
    const log: string[] = [];
    const gap = "Some servers didn't answer, so an address may be missing. Try again once they do.";
    const end = await Effect.runPromise(runSwitch("tunnel", ports((doors) => answer(doors), log, { seen: { failure: gap } })));
    expect(end).toEqual({ ok: false, failure: gap });
    expect(log).toEqual(["at opening", "open public+tunnel", "at moving", "moving"]);
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

  const known = (...routes: [string, string, "generated" | "custom"][]): KnownRoutes =>
    new Map(routes.map(([namespace, hostname, kind]) => [routeKey(namespace, hostname), kind]));

  it("moves each generated hostname one label below the new source, and leaves custom domains alone", () => {
    const routes = known(["shop-prod", "web-prod.acme.ployz.app", "generated"], ["shop-prod", "deep.web-prod.acme.ployz.app", "generated"], ["shop-prod", "www.example.org", "custom"]);
    expect(movedHostnames(published, "acme.ployz.app", "example.com", routes)).toEqual([
      { from: "web-prod.acme.ployz.app", to: "web-prod.example.com", namespace: "shop-prod", service: "web" },
    ]);
    expect(unknownHostnames(published, "acme.ployz.app", routes)).toEqual([]);
  });

  it("tells a custom domain spelled like a generated name by the Deployment that put it there, not its shape, and lists it as staying", () => {
    const back = [
      { hostname: "web-prod.example.com", namespace: "shop-prod", service: "web" },
      { hostname: "shop.example.com", namespace: "shop-prod", service: "shop" },
      { hostname: "www.example.org", namespace: "shop-prod", service: "web" },
    ];
    const routes = known(["shop-prod", "web-prod.example.com", "generated"], ["shop-prod", "shop.example.com", "custom"], ["shop-prod", "www.example.org", "custom"]);
    expect(movedHostnames(back, "example.com", "acme.ployz.app", routes)).toEqual([
      { from: "web-prod.example.com", to: "web-prod.acme.ployz.app", namespace: "shop-prod", service: "web" },
    ]);
    expect(keptHostnames(back, routes)).toEqual(["shop.example.com", "www.example.org"]);
    expect(keptHostnames(back, new Map())).toEqual([]);
  });

  it("never moves a name no Deployment accounts for, in another Namespace or none, and names the first as why the switch stops", () => {
    const back = [
      { hostname: "web-prod.example.com", namespace: "shop-prod", service: "web" },
      { hostname: "web-prod.example.com", namespace: "old-prod", service: "web" },
      { hostname: "deep.stray.example.com", namespace: "shop-prod", service: "web" },
    ];
    const routes = known(["shop-prod", "web-prod.example.com", "generated"]);
    expect(movedHostnames(back, "example.com", "acme.ployz.app", routes)).toEqual([
      { from: "web-prod.example.com", to: "web-prod.acme.ployz.app", namespace: "shop-prod", service: "web" },
    ]);
    const unknown = unknownHostnames(back, "example.com", routes);
    expect(unknown).toEqual([{ hostname: "web-prod.example.com", namespace: "old-prod", service: "web" }]);
    expect(unknownGap(unknown)).toBe("web-prod.example.com is served by old-prod/web, which Ployz Cloud can't match to a deployment. Deploy it again or remove it, then switch again.");
    expect(unknownGap([])).toBeNull();
    expect(movedHostnames(back, "example.com", "acme.ployz.app", new Map())).toEqual([]);
  });

  it("names a move whose new address a custom domain already holds, whichever Service holds it", () => {
    const moving = [
      { from: "wk.acme.ployz.app", to: "wk.example.com", namespace: "shop-prod", service: "worker" },
      { from: "api.acme.ployz.app", to: "api.example.com", namespace: "shop-prod", service: "api" },
    ];
    const routes = known(
      ["shop-prod", "wk.acme.ployz.app", "generated"], ["shop-prod", "wk.example.com", "custom"],
      ["shop-prod", "api.acme.ployz.app", "generated"], ["docs-main", "api.example.org", "custom"],
    );
    expect(collidingMoves(moving, routes)).toEqual([moving[0]]);
    expect(collisionGap(collidingMoves(moving, routes))).toBe(
      "wk.example.com would be the generated domain of shop-prod/worker, and a custom domain already holds it. Remove the generated domain or the custom domain, then switch again.",
    );
    expect(collidingMoves(moving, known(["docs-main", "api.example.com", "custom"]))).toEqual([moving[1]]);
    expect(collidingMoves(moving, new Map())).toEqual([]);
    expect(collisionGap([])).toBeNull();
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
        { namespace: "shop-prod", project: "shop", environment: "production", services: ["api", "web"] },
        { namespace: "docs-main", project: "docs", environment: "main", services: ["site"] },
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

describe("what the Servers showed", () => {
  const seen = (partial: boolean, ...hostnames: string[]) => ({
    servers: [], hostnames: hostnames.map((hostname) => ({ hostname, namespace: "shop-prod", service: "web" })), partial,
  });

  it("names what stops a switch from trusting one look: no Server answered, or not every one", () => {
    expect(observationGap(null)).toBe("Your servers didn't answer. Try again once they do.");
    expect(observationGap(seen(true))).toBe("Some servers didn't answer, so an address may be missing. Try again once they do.");
    expect(observationGap(seen(false))).toBeNull();
  });

  it("finds the moves whose new address its Namespace doesn't publish yet", () => {
    expect(unservedMoves(seen(false, "web-prod.acme.ployz.app").hostnames, [MOVE])).toEqual([MOVE]);
    expect(unservedMoves(seen(false, "web-prod.example.com").hostnames, [MOVE])).toEqual([]);
    expect(unservedMoves([{ hostname: "web-prod.example.com", namespace: "other", service: "web" }], [MOVE])).toEqual([MOVE]);
  });
});

describe("redeploying what moves", () => {
  const web = { namespace: "shop-prod", project: "shop", environment: "production", services: ["web"] };
  const scope = [web];
  const both = [{ ...web, services: ["web", "worker"] }];
  const fast = { poll: "5 millis", limit: "40 millis" } as const;
  /** A Store whose Deployments end as `status`, and Servers publishing `served` on each look. */
  const ports = (
    status: "applied" | "superseded" | "failed", served: ReadonlyArray<ReadonlyArray<string> | null>, log: string[], refusesTogether = false,
  ): RedeployPorts<never, never> => {
    let looks = 0;
    return {
      reapply: ({ project, environment, services }) => Effect.sync(() => {
        log.push(`reapply ${project}/${environment} ${services.join(",")}`);
        return refusesTogether && services.length > 1 ? { refused: "web, worker can't redeploy as they run now." } : { id: "d1" };
      }),
      ended: () => Effect.sync(() => { log.push(`ended ${status}`); return status; }),
      observe: () => Effect.sync(() => {
        const now = served[Math.min(looks, served.length - 1)] ?? null;
        looks += 1;
        return now === null ? null : { servers: [], hostnames: now.map((hostname) => ({ hostname, namespace: "shop-prod", service: "web" })), partial: false };
      }),
    };
  };

  it("ends once the Servers publish every moved address, however the Deployment ended", async () => {
    const log: string[] = [];
    expect(await Effect.runPromise(redeployMoved([MOVE], scope, ports("superseded", [["web-prod.acme.ployz.app"], ["web-prod.example.com"]], log), fast))).toBeNull();
    expect(log).toEqual(["reapply shop/production web", "ended superseded"]);
  });

  it("doesn't take a superseded Deployment as proof: an address never served names its Environment", async () => {
    const end = await Effect.runPromise(redeployMoved([MOVE], scope, ports("superseded", [["web-prod.acme.ployz.app"]], []), fast));
    expect(end).toBe("web-prod.example.com isn't served yet. Check the deployments of shop/production, then switch again.");
  });

  it("says why when the Servers stop answering after the Deployment, instead of taking that as served", async () => {
    const end = await Effect.runPromise(redeployMoved([MOVE], scope, ports("applied", [null], []), fast));
    expect(end).toBe("Your servers didn't answer. Try again once they do.");
  });

  it("stops at a Deployment that failed, without looking at the Servers", async () => {
    const log: string[] = [];
    const end = await Effect.runPromise(redeployMoved([MOVE], scope, ports("failed", [["web-prod.example.com"]], log), fast));
    expect(end).toBe("shop/production didn't redeploy. Check its deployments, then switch again.");
    expect(log).toEqual(["reapply shop/production web", "ended failed"]);
  });

  it("reapplies one Service at a time once the Store refuses them together", async () => {
    const log: string[] = [];
    expect(await Effect.runPromise(redeployMoved([MOVE], both, ports("applied", [["web-prod.example.com"]], log, true), fast))).toBeNull();
    expect(log).toEqual(["reapply shop/production web,worker", "reapply shop/production web", "ended applied", "reapply shop/production worker", "ended applied"]);
  });

  it("stops at the first Service that doesn't reapply alone", async () => {
    const log: string[] = [];
    const end = await Effect.runPromise(redeployMoved([MOVE], both, ports("failed", [["web-prod.example.com"]], log, true), fast));
    expect(end).toBe("shop/production didn't redeploy. Check its deployments, then switch again.");
    expect(log).toEqual(["reapply shop/production web,worker", "reapply shop/production web", "ended failed"]);
  });

  it("doesn't reapply one at a time when the Deployment of them together fails", async () => {
    const log: string[] = [];
    await Effect.runPromise(redeployMoved([MOVE], both, ports("failed", [["web-prod.example.com"]], log), fast));
    expect(log).toEqual(["reapply shop/production web,worker", "ended failed"]);
  });

  it("deploys nothing when nothing moves", async () => {
    const log: string[] = [];
    expect(await Effect.runPromise(redeployMoved([], scope, ports("applied", [], log), fast))).toBeNull();
    expect(log).toEqual([]);
  });
});

describe("when the Cluster can switch", () => {
  const setup = (state: "set_up" | "active" | "left"): CloudflareSetup => ({ zone: "example.com", tunnelName: "ployz-acme", tunnelId: "t1", state });
  const settingUp: CloudflareSetup = { zone: "example.com", tunnelName: "ployz-acme", tunnelId: null, state: "setting_up" };

  it("goes to Cloudflare only once it is set up and not yet in use", () => {
    expect(switchProblem(null, "tunnel")).toBe("Set up Cloudflare first, then switch to it.");
    expect(switchProblem(setup("set_up"), "tunnel")).toBeNull();
    expect(switchProblem(setup("active"), "tunnel")).toBe("Your servers already use Cloudflare Tunnel.");
    expect(switchProblem(setup("left"), "tunnel")).toBe("Set up Cloudflare again, then switch to it.");
  });

  it("goes back to public ports from anywhere but public ports", () => {
    expect(switchProblem(setup("active"), "public")).toBeNull();
    expect(switchProblem(setup("left"), "public")).toBe("Your servers already use public ports.");
  });

  it("refuses both ways while a setup holds the Cluster", () => {
    expect(switchProblem(settingUp, "tunnel")).toBe("Cloudflare is being set up. Wait for it to finish, then switch.");
    expect(switchProblem(settingUp, "public")).toBe("Cloudflare is being set up. Wait for it to finish, then switch.");
  });

  const ended = (want: "tunnel" | "public", state: "failed" | "finished"): LatestDoorsChange => state === "failed"
    ? { attemptId: "a", want, state, endedAt: "2026-11-08T00:00:00Z", failure: "s1 didn't switch." }
    : { attemptId: "a", want, state, endedAt: "2026-11-08T00:00:00Z" };

  it("lets a switch whose source moved but whose last step failed be asked again, and only that one", () => {
    expect(switchProblem(setup("active"), "tunnel", ended("tunnel", "failed"))).toBeNull();
    expect(switchProblem(setup("active"), "tunnel", ended("tunnel", "finished"))).toBe("Your servers already use Cloudflare Tunnel.");
    expect(switchProblem(setup("active"), "tunnel", ended("public", "failed"))).toBe("Your servers already use Cloudflare Tunnel.");
    expect(switchProblem(setup("left"), "public", ended("public", "failed"))).toBeNull();
    expect(switchProblem(setup("left"), "public", ended("tunnel", "failed"))).toBe("Your servers already use public ports.");
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

  it("never calls a Door open or closed on every Server from a look that missed some", () => {
    const partial = (...doors: ReadonlyArray<readonly ("public" | "tunnel")[]>) => ({
      servers: doors.map((held, index) => ({ id: `m${index + 1}`, name: `s${index + 1}`, doors: held })), hostnames: [], partial: true,
    });
    expect(doorWords(partial(["tunnel"]), "tunnel")).toBe("Open on 1 of 1 servers. Some servers didn't answer.");
    expect(doorWords(partial(["tunnel"]), "public")).toBe("Open on 0 of 1 servers. Some servers didn't answer.");
    expect(doorWords(partial(), "public")).toBe("Some servers didn't answer.");
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
