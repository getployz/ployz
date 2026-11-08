import type { RuntimeWatchView } from "@ployz/sdk";
import { Effect } from "effect";
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

it("looks only at the hostnames outside the zone, since its wildcard covers the rest, and names the tunnel's target", async () => {
  const looked: string[] = [];
  const reach = (hostname: string) => Effect.sync(() => { looked.push(hostname); return hostname === "shop.example.org"; });
  const evidence = await Effect.runPromise(tunnelEvidence(
    "org-1", cloudflare, observed("web.acme-apps.com", "a.b.acme-apps.com", "shop.example.org", "acme-apps.com", "wrong.example.net"), true, reach,
  ));
  expect(evidence).toEqual({
    target: "0a1b2c.cfargotunnel.com",
    reached: [
      { hostname: "shop.example.org", reached: true },
      { hostname: "acme-apps.com", reached: false },
      { hostname: "wrong.example.net", reached: false },
    ],
  });
  // A look-alike suffix is another domain, not the zone's.
  expect(looked).not.toContain("web.acme-apps.com");
  const lookalike = await Effect.runPromise(tunnelEvidence("org-1", cloudflare, observed("shopacme-apps.com"), true, (hostname) => Effect.succeed(hostname === "x")));
  expect(lookalike.reached).toEqual([{ hostname: "shopacme-apps.com", reached: false }]);
});

it("leaves a hostname out when a fetch couldn't judge it, and says nothing when no Server was observed", async () => {
  const evidence = await Effect.runPromise(tunnelEvidence("org-2", cloudflare, observed("shop.example.org", "blog.example.org"), true, (hostname) => Effect.succeed(hostname === "blog.example.org" ? true : null)));
  expect(evidence.reached).toEqual([{ hostname: "blog.example.org", reached: true }]);
  const nothingSeen = await Effect.runPromise(tunnelEvidence("org-2", cloudflare, null, true, () => Effect.die("never fetches")));
  expect(nothingSeen).toEqual({ target: "0a1b2c.cfargotunnel.com", reached: [] });
});

it("reuses a fetch from seconds ago for a read and fetches again for a check", async () => {
  let fetched = 0;
  const reach = () => Effect.sync(() => { fetched += 1; return true; });
  await Effect.runPromise(tunnelEvidence("org-3", cloudflare, observed("shop.example.org"), true, reach));
  await Effect.runPromise(tunnelEvidence("org-3", cloudflare, observed("shop.example.org"), false, reach));
  expect(fetched).toBe(1);
  await Effect.runPromise(tunnelEvidence("org-3", cloudflare, observed("shop.example.org"), true, reach));
  expect(fetched).toBe(2);
  // Another Organization's answer is its own.
  await Effect.runPromise(tunnelEvidence("org-4", cloudflare, observed("shop.example.org"), false, reach));
  expect(fetched).toBe(3);
});
