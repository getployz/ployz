import type { RuntimeWatchView, TunnelAnswer } from "@ployz/sdk";
import { Deferred, Effect, Fiber } from "effect";
import { TestClock } from "effect/testing";
import { expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { type Observed, observedOf, publishedOf, tunnelEvidence } from "./domain-evidence.server";

it("reads the hostnames the Servers publish, once per Namespace, with who publishes them, and never a hook's", () => {
  const container = (namespace: string, name: string, ports: unknown[], kind = "service_container") => ({ namespace, kind, resolved_spec: { name, ports } });
  const ingress = (hostname: string) => ({ mode: "ingress", hostname });
  const frame = asTestDouble<RuntimeWatchView>()({ containers: [
    container("shop-prod", "web", [ingress("shop.ployz.app"), { mode: "host" }]),
    container("shop-prod", "web", [ingress("shop.ployz.app")]),
    container("old", "web", [ingress("shop.ployz.app")]),
    // A hook keeps its Service's ports but serves nothing.
    container("hooked", "web", [ingress("blog.ployz.app")], "pre_deploy_hook"),
  ] } as never);
  expect(publishedOf(frame)).toEqual([
    { hostname: "shop.ployz.app", namespace: "shop-prod", service: "web" },
    { hostname: "shop.ployz.app", namespace: "old", service: "web" },
  ]);
});

it("counts the ingress Servers apart from their addresses, and passes on the certificates the frame couldn't read", () => {
  const machine = (accepts_ingress: boolean, public_ip: string | null) => ({ machine: { accepts_ingress, public_ip } });
  const frame = asTestDouble<RuntimeWatchView>()({
    machines: [machine(true, "203.0.113.7"), machine(true, null), machine(false, "203.0.113.8")],
    containers: [],
    certificates: [],
    incomplete_ids: { machines: [], containers: [], volumes: [], certificates: ["app.example.com"] },
  } as never);
  expect(observedOf(frame)).toMatchObject({
    ingress_servers: 2,
    ingress_addresses: ["203.0.113.7"],
    incomplete_certificates: ["app.example.com"],
  });
});

it("doesn't count the ingress Servers while a Machine row is unread, which could be one of them", () => {
  const frame = asTestDouble<RuntimeWatchView>()({
    machines: [],
    containers: [],
    certificates: [],
    incomplete_ids: { machines: ["machine-1"], containers: [], volumes: [], certificates: [] },
  } as never);
  expect(observedOf(frame).ingress_servers).toBeUndefined();
});

const published = (...hostnames: string[]) => hostnames.map((hostname) => ({ hostname, namespace: "shop-prod", service: "web" }));
const observed = (...hostnames: string[]) => asTestDouble<Observed>()({
  seen: { published: published(...hostnames), ingress_addresses: ["203.0.113.7"] },
  machineIds: new Set(["machine-1"]),
} as never);
const cloudflare = { zone: "acme-apps.com", target: "0a1b2c.cfargotunnel.com" };
const answering = (cluster: string) => (hostname: string) => Effect.succeed<TunnelAnswer>(hostname === cluster ? "cluster" : "elsewhere");

it("fetches through each custom hostname the read judges that the Servers publish, inside the zone too, and names the tunnel's target", async () => {
  const looked: string[] = [];
  const reach = (hostname: string) => Effect.sync((): TunnelAnswer => { looked.push(hostname); return hostname === "shop.example.org" ? "cluster" : "elsewhere"; });
  const custom = ["shop.acme-apps.com", "shop.example.org", "acme-apps.com", "a.b.acme-apps.com", "unpublished.example.net"];
  const evidence = await Effect.runPromise(tunnelEvidence(
    "org-1", cloudflare, observed("web.acme-apps.com", "shop.acme-apps.com", "a.b.acme-apps.com", "shop.example.org", "acme-apps.com"), true, custom, reach,
  ));
  expect(evidence).toEqual({
    target: "0a1b2c.cfargotunnel.com",
    reached: [
      { hostname: "shop.acme-apps.com", answer: "elsewhere" },
      { hostname: "shop.example.org", answer: "cluster" },
      { hostname: "acme-apps.com", answer: "elsewhere" },
      { hostname: "a.b.acme-apps.com", answer: "elsewhere" },
    ],
  });
  // A generated name is the zone's wildcard's to serve, and a name no Server publishes isn't live to fetch through.
  expect(looked).not.toContain("web.acme-apps.com");
  expect(looked).not.toContain("unpublished.example.net");
});

it("reports a fetch nobody answered as that, and says nothing when no Server was observed", async () => {
  const evidence = await Effect.runPromise(tunnelEvidence(
    "org-2", cloudflare, observed("shop.example.org", "blog.example.org"), true, ["shop.example.org", "blog.example.org"],
    (hostname) => Effect.succeed<TunnelAnswer>(hostname === "blog.example.org" ? "cluster" : "unanswered"),
  ));
  expect(evidence.reached).toEqual([{ hostname: "shop.example.org", answer: "unanswered" }, { hostname: "blog.example.org", answer: "cluster" }]);
  const nothingSeen = await Effect.runPromise(tunnelEvidence("org-2", cloudflare, null, true, ["shop.example.org"], () => Effect.die("never fetches")));
  expect(nothingSeen).toEqual({ target: "0a1b2c.cfargotunnel.com", reached: [] });
});

it("reuses a fetch from seconds ago for a read, a failed one too, and fetches again for a check", async () => {
  let fetched = 0;
  const reach = (hostname: string) => Effect.sync((): TunnelAnswer => { fetched += 1; return hostname === "dead.example.org" ? "unanswered" : "cluster"; });
  const read = (organization: string, fresh: boolean, ...hostnames: string[]) =>
    Effect.runPromise(tunnelEvidence(organization, cloudflare, observed(...hostnames), fresh, hostnames, reach));
  await read("org-3", true, "shop.example.org", "dead.example.org");
  await read("org-3", false, "shop.example.org", "dead.example.org");
  expect(fetched).toBe(2);
  await read("org-3", true, "shop.example.org");
  expect(fetched).toBe(3);
  // Another Organization's answer is its own.
  await read("org-4", false, "shop.example.org");
  expect(fetched).toBe(4);
});

it("shares one fetch between reads that overlap, and fetches through a bounded number of hostnames", async () => {
  const gate = await Effect.runPromise(Deferred.make<TunnelAnswer>());
  const started = await Effect.runPromise(Deferred.make<void>());
  let fetched = 0;
  const reach = () => Effect.suspend(() => { fetched += 1; return Deferred.succeed(started, undefined).pipe(Effect.andThen(Deferred.await(gate))); });
  const read = (fresh: boolean) => tunnelEvidence("org-5", cloudflare, observed("shop.example.org"), fresh, ["shop.example.org"], reach);
  const both = Effect.runPromise(Effect.all([read(true), read(true), read(false)], { concurrency: "unbounded" }));
  await Effect.runPromise(Deferred.await(started));
  await Effect.runPromise(Deferred.succeed(gate, "cluster"));
  for (const evidence of await both) expect(evidence.reached).toEqual([{ hostname: "shop.example.org", answer: "cluster" }]);
  expect(fetched).toBe(1);
  const many = Array.from({ length: 40 }, (_, index) => `site-${index}.example.org`);
  const evidence = await Effect.runPromise(tunnelEvidence("org-6", cloudflare, observed(...many), true, many, answering("none")));
  expect(evidence.reached).toHaveLength(16);
});

it("answers every name it has an answer for, spends the fetch budget on the ones never observed, and reaches the rest on later reads", async () => {
  const many = Array.from({ length: 17 }, (_, index) => `site-${String(index).padStart(2, "0")}.example.org`);
  let fetched = 0;
  const reach = () => Effect.sync((): TunnelAnswer => { fetched += 1; return "cluster"; });
  const read = () => Effect.runPromise(tunnelEvidence("org-8", cloudflare, observed(...many), false, many, reach));
  expect((await read()).reached).toHaveLength(16);
  expect(fetched).toBe(16);
  const second = await read();
  expect(second.reached.map(({ hostname }) => hostname).sort()).toEqual([...many].sort());
  expect(fetched).toBe(17);
});

it("waits a bounded time for a first fetch, serves a stale answer while a fetch refreshes it, and never holds a read on a slow name", () => Effect.gen(function* () {
  let gate = yield* Deferred.make<TunnelAnswer>();
  let fetched = 0;
  const reach = (hostname: string) => Effect.suspend(() => {
    fetched += 1;
    return hostname === "slow.example.org" ? Deferred.await(gate) : Effect.succeed<TunnelAnswer>("cluster");
  });
  const names = ["fast.example.org", "slow.example.org"];
  const read = () => tunnelEvidence("org-7", cloudflare, observed(...names), false, names, reach);
  const first = yield* Effect.forkChild(read());
  yield* TestClock.adjust("250 millis");
  expect((yield* Fiber.join(first)).reached).toEqual([{ hostname: "fast.example.org", answer: "cluster" }]);
  yield* Deferred.succeed(gate, "elsewhere");
  yield* Effect.yieldNow;
  const settled = [{ hostname: "fast.example.org", answer: "cluster" }, { hostname: "slow.example.org", answer: "elsewhere" }];
  expect((yield* read()).reached).toEqual(settled);
  expect(fetched).toBe(2);
  gate = yield* Deferred.make<TunnelAnswer>();
  yield* TestClock.adjust("16 seconds");
  expect((yield* read()).reached).toEqual(settled);
  expect(fetched).toBe(4);
}).pipe(Effect.provide(TestClock.layer()), Effect.runPromise));
