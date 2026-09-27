import { describe, expect, it } from "vitest";
import { parseServiceConfig } from "@ployz/sdk/config";
import type { SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { branchNameError, defaultBranchName, offeredPresets, planBranchOf, presetSummary } from "./branch-plan";

const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const [WEB, DB, CACHE, DATA] = [id(1), id(2), id(3), id(4)];
const names = new Map([[WEB, "web"], [DB, "postgres"], [CACHE, "cache"], [DATA, "postgres-data"]]);

function service(n: number, lineageId: string, slug: string) {
  const { env: _env, mounts: _mounts, ...config } = parseServiceConfig({
    version: 2, source: { version: 1, type: "image", image: `${slug}:1`, credentials: { type: "none" } },
    healthcheck: { type: "none" }, restartPolicy: "unless-stopped", privateDns: slug,
  });
  return { id: id(10 + n), lineageId, slug, config, variables: [], volumeAttachments: [] } as SavedEnvironmentIntent["services"][number];
}

/** web uses postgres, which mounts postgres-data; cache stands alone. */
function parent(): SavedEnvironmentIntent {
  const web = service(1, WEB, "web");
  web.variables = [{ id: id(20), key: "DB", description: null, exported: false, valueFingerprint: "fp",
    value: { kind: "template", parts: [{ kind: "ref", owner: { scope: "service", lineageId: DB }, key: "URL" }] } }];
  const db = service(2, DB, "postgres");
  db.volumeAttachments = [{ volumeResourceId: id(30), mountPath: "/data" }];
  return { version: 1, environmentSlug: "shop-production", services: [web, db, service(3, CACHE, "cache")],
    volumes: [{ resourceId: id(30), resourceLineageId: DATA, name: "postgres-data" }] };
}

const nameOf = (lineage: string) => names.get(lineage) ?? lineage;

describe("branch plan", () => {
  const input = { parent: parent(), deployed: [WEB, DB, CACHE, DATA], focus: [WEB] };

  it("describes each preset in words and offers Plus what it uses only when it adds something", () => {
    expect(offeredPresets(input)).toEqual(["only", "uses", "all"]);
    expect(offeredPresets({ ...input, focus: [CACHE] })).toEqual(["only", "all"]);
    const only = planBranchOf({ ...input, picks: { preset: "only" } });
    const uses = planBranchOf({ ...input, picks: { preset: "uses" } });
    expect(presetSummary("only", only, only, nameOf, "production"))
      .toBe("web gets its own copy. What it uses comes from production, live.");
    expect(presetSummary("uses", uses, only, nameOf, "production"))
      .toBe("postgres and postgres-data get copies too, so nothing touches production's data.");
    expect(presetSummary("all", uses, only, nameOf, "production")).toBe("A full copy of production.");
    expect(presetSummary("only", planBranchOf({ ...input, focus: [], picks: { preset: "only" } }), only, nameOf, "production"))
      .toBe("Tick what changes below.");
  });

  it("reads hand picks that match no preset as picked by hand", () => {
    expect(planBranchOf({ ...input, picks: { own: [WEB] } }).preset).toBe("only");
    expect(planBranchOf({ ...input, picks: { own: [WEB, CACHE] } }).preset).toBeNull();
  });

  it("flags a taken or too-long name and defaults to a free one", () => {
    const taken = new Set(["shop-new-branch"]);
    expect(branchNameError("shop", "new-branch", taken)).toBe("new-branch is taken in this organization.");
    expect(branchNameError("shop", "x".repeat(60), taken)).toBe("Too long: shorten the name.");
    expect(branchNameError("shop", " ", taken)).toBe("Name the branch.");
    expect(defaultBranchName("shop", "new-branch", taken)).toBe("new-branch-2");
  });
});
