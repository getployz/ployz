import type { DiffView, ServiceListing } from "@ployz/sdk";
import { expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { changeGroups, deploymentActions, nodeLight, previewLines, uploadLabel } from "./store-deployments";

// The grouping reads only the changes and each Service's id and source.
const diff = asTestDouble<DiffView>()({
  total_count: 3,
  changes: [
    {
      type: "service", id: "s1", name: "web", lifecycle: "update", comparison: "head", data: null,
      settings: [
        { path: "web.replicas", kind: "update", before: 1, after: 2, canRestore: true },
        { path: "web.env.TOKEN", kind: "add", before: null, after: "abc", canRestore: true },
        { path: "web.mounts.pg-data", kind: "remove", before: "/data", after: null, canRestore: true },
      ],
    },
    { type: "volume", id: "v1", name: "pg-data", lifecycle: "create", comparison: null, data: null, settings: [] },
  ],
});
const services = [asTestDouble<ServiceListing>()({ id: "s1", source: "image" })];

it("groups the Store's review by node, labelling rows from the catalog and discarding only Services' changes", () => {
  const [web, volume] = changeGroups(diff, services);
  expect(web).toMatchObject({ nodeType: "service", nodeName: "web", lifecycle: "update", canDiscard: true, serviceSourceType: "image", changeCount: 3 });
  expect(web?.rows.map((row) => [row.path, row.label, row.currentValue, row.newValue, row.canDiscard])).toEqual([
    ["web.replicas", "Replicas", "1", "2", true],
    // Variables and mounts discard only with their Service.
    ["web.env.TOKEN", "Environment variable TOKEN", "", "abc", false],
    ["web.mounts.pg-data", "Volume mount pg-data", "/data", "", false],
  ]);
  // The Store can't discard a Volume node.
  expect(volume).toMatchObject({ nodeType: "volume", lifecycle: "create", canDiscard: false, changeCount: 1, rows: [] });
});

it("reads a pending node as its Deployment does, and a vanished runner's node as Unknown, never Failed", () => {
  expect(nodeLight("pending", "queued")).toBe("queued");
  expect(nodeLight("pending", "running")).toBe("deploying");
  expect(nodeLight("pending", "cancelled")).toBe("not_applied");
  expect(nodeLight("applied", "failed")).toBe("deployed");
  expect(nodeLight("unknown", "unknown")).toBe("unknown");
});

it("offers retry only after a Deployment ended without applying, start while queued, cancel before it ends", () => {
  expect(deploymentActions("failed")).toEqual({ retry: true, start: false, cancel: false });
  expect(deploymentActions("queued")).toEqual({ retry: false, start: true, cancel: true });
  expect(deploymentActions("running")).toEqual({ retry: false, start: false, cancel: true });
  expect(deploymentActions("cancelling")).toEqual({ retry: false, start: false, cancel: false });
  expect(deploymentActions("applied")).toEqual({ retry: false, start: false, cancel: false });
});

it("names an upload's provenance", () => {
  expect(uploadLabel({ digest: "d", base: { commit: "abc1234def", changed: true }, uploader: "nick" })).toBe("Uploaded by nick · abc1234 + changes");
  expect(uploadLabel({ digest: "d", base: null })).toBe("Uploaded");
});

it("summarizes a recorded Deploy Preview, and nothing before one", () => {
  expect(previewLines(null)).toBeNull();
  expect(previewLines({
    namespace: "shop-production", storage: [], preserved_volumes: [], prune_refusal: null,
    operations: [{ index: 0, service_name: "web", status: { type: "pending" } }, { index: 1, service_name: "web", status: { type: "pending" } },
      { index: 2, service_name: null, status: { type: "pending" } }],
    volumes_to_create: [{}], would_remove: [{}],
    warnings: [{ type: "ingress_hostname", message: "shop.example.com points elsewhere" }, { type: "unbudgeted_disk_usage" }],
  })).toEqual([
    "3 operations: web 2, Environment 1", "Creates 1 volume", "Removes 1 service", "shop.example.com points elsewhere", "unbudgeted disk usage",
  ]);
});
