import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import type { EnvironmentChangeStateNodeProjection, EnvironmentChangeStateProjection } from "#/modules/deployments/deployment-contract";
import { createImageServiceSource } from "#/modules/environment-design/services";
import { deletionNodes } from "./deletion-items";

const projects = [{ id: "p-shop", slug: "shop", name: "shop" }];
const environments = [
  { id: "e-fix", projectId: "p-shop", namespace: "shop-fix-api", name: "fix-api" },
  { id: "e-prod", projectId: "p-shop", namespace: "shop-production", name: "production" },
];
const image = createImageServiceSource({ image: "ghcr.io/acme/api:1.5.0" });
/** An Environment whose Applied State runs one service. */
const running = (environmentId: string, nodeId: string, lineage: string) =>
  asTestDouble<Pick<EnvironmentChangeStateProjection, "environmentId" | "applied">>()({
    environmentId,
    applied: {
      nodes: [asTestDouble<EnvironmentChangeStateNodeProjection>()({
        nodeType: "service", nodeId, nodeLineageId: lineage, revisionId: null, config: { source: image },
      })],
    },
  });
const names = new Map([["l-api", "api"], ["l-web", "web"]]);
const nameOf = (lineage: string) => names.get(lineage) ?? "a node";

describe("deletionNodes", () => {
  it("keeps a service whose delete is staged: it runs until the next deploy", () => {
    const items = deletionNodes({
      within: { projectSlug: "shop", environmentSlug: "shop-fix-api" }, projects, environments, nameOf,
      applied: [running("e-fix", "s-api", "l-api")], services: [], volumes: [],
    });
    expect(items.map(({ kind, name, detail }) => ({ kind, name, detail }))).toEqual([{ kind: "service", name: "api", detail: undefined }]);
  });

  it("names a node as authored, and leaves out what runs outside the scope", () => {
    const items = deletionNodes({
      within: { projectSlug: "shop", environmentSlug: "shop-fix-api" }, projects, environments, nameOf,
      applied: [running("e-fix", "s-api", "l-api"), running("e-prod", "s-web", "l-web")],
      services: [{ id: "s-api", name: "api-v2", source: image, projectSlug: "shop", environmentSlug: "shop-fix-api" }],
      volumes: [],
    });
    expect(items.map((item) => item.name)).toEqual(["api-v2"]);
  });

  it("says which Environment each node is in, past one Environment", () => {
    const items = deletionNodes({
      within: { projectSlug: "shop" }, projects, environments, nameOf,
      applied: [running("e-prod", "s-web", "l-web")], services: [],
      volumes: [{ id: "v-data", name: "pg-data", projectSlug: "shop", environmentSlug: "shop-production" }],
    });
    expect(items.map(({ name, detail }) => `${name} · ${detail}`)).toEqual(["web · production", "pg-data · production"]);
  });
});
