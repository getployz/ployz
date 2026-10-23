import type { RuntimeWatchView } from "@ployz/sdk";
import { expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { observedOf, publishedOf } from "./domain-evidence.server";

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
