import type { RuntimeWatchView } from "@ployz/sdk";
import { expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { publishedOf } from "./domain-evidence.server";

it("reads the hostnames the Servers publish, once per Namespace, with who publishes them", () => {
  const container = (namespace: string, name: string, ports: unknown[]) => ({ namespace, resolved_spec: { name, ports } });
  const ingress = (hostname: string) => ({ mode: "ingress", hostname });
  const frame = asTestDouble<RuntimeWatchView>()({ containers: [
    container("shop-prod", "web", [ingress("shop.ployz.app"), { mode: "host" }]),
    container("shop-prod", "web", [ingress("shop.ployz.app")]),
    container("old", "web", [ingress("shop.ployz.app")]),
  ] } as never);
  expect(publishedOf(frame)).toEqual([
    { hostname: "shop.ployz.app", namespace: "shop-prod", service: "web" },
    { hostname: "shop.ployz.app", namespace: "old", service: "web" },
  ]);
});
