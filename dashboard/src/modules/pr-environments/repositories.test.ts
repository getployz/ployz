import { describe, expect, it } from "vitest";
import { parseServiceConfig, type ServiceSource } from "@ployz/sdk/config";
import type { SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { createGitServiceSource, createImageServiceSource } from "#/modules/environment-design/services";
import { planSummary, prPlanInput } from "./repositories";

const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const [WEB, DB, CACHE, DATA] = [id(1), id(2), id(3), id(4)];
const names = new Map([[WEB, "web"], [DB, "postgres"], [CACHE, "cache"], [DATA, "postgres-data"]]);
const nameOf = (lineage: string) => names.get(lineage) ?? lineage;

function service(n: number, lineageId: string, slug: string, source: ServiceSource) {
  const { env: _env, mounts: _mounts, ...config } = parseServiceConfig({
    version: 2, source, healthcheck: { type: "none" }, restartPolicy: "unless-stopped", privateDns: slug,
  });
  return { id: id(10 + n), lineageId, slug, config, variables: [], volumeAttachments: [] } as SavedEnvironmentIntent["services"][number];
}

/** web deploys from acme/app and uses postgres, which mounts postgres-data; cache stands alone. */
function staging(): SavedEnvironmentIntent {
  const web = service(1, WEB, "web", createGitServiceSource({ repository: "acme/app", repositoryId: 42,
    access: { type: "github-installation", installationId: 7 }, branch: { type: "connected", name: "dev" } }));
  web.variables = [{ id: id(20), key: "DB", description: null, exported: false, valueFingerprint: "fp",
    value: { kind: "template", parts: [{ kind: "ref", owner: { scope: "service", lineageId: DB }, key: "URL" }] } }];
  const db = service(2, DB, "postgres", createImageServiceSource({ image: "postgres:16" }));
  db.volumeAttachments = [{ volumeResourceId: id(30), mountPath: "/data" }];
  return { version: 1, environmentSlug: "shop-staging", services: [web, db, service(3, CACHE, "cache", createImageServiceSource({ image: "redis:7" }))],
    volumes: [{ resourceId: id(30), resourceLineageId: DATA, name: "postgres-data" }] };
}

const deployed = [WEB, DB, CACHE, DATA];
const on = { enabled: true, setupCommands: [{ lineageId: WEB, command: "php artisan migrate --seed" }] };
const summary = (picks: Parameters<typeof prPlanInput>[3]) => planSummary(on, "staging", prPlanInput(staging(), deployed, 42, picks), nameOf);

describe("PR Environments plan", () => {
  it("keeps the repository's services as Own Copies and drops hand picks the start-from lacks", () => {
    const input = prPlanInput(staging(), deployed, 42, { own: [CACHE, id(99)] });
    expect(input.focus).toEqual([WEB]);
    expect(input.picks).toEqual({ own: [WEB, CACHE] });
    expect(prPlanInput(staging(), deployed, 42, { preset: "uses" }).picks).toEqual({ preset: "uses" });
  });

  it("says in words what a PR Environment gets", () => {
    expect(summary({ preset: "only" })).toBe("On · from staging · postgres used live");
    expect(summary({ preset: "uses" }))
      .toBe("On · from staging · postgres and postgres-data copied · empty data · then php artisan migrate --seed");
    expect(summary({ preset: "all" })).toBe("On · from staging · everything copied · empty data · then php artisan migrate --seed");
    expect(planSummary({ ...on, enabled: false }, "staging", null, nameOf)).toBe("Off");
    expect(planSummary(on, undefined, null, nameOf)).toBe("On · pick an environment to start from");
  });
});
