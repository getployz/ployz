import type { ConfigStore, MachineId } from "@ployz/sdk";
import { Effect } from "effect";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { discardPlacement, listPlacements, stagePlacement, stampPlacements } from "#/modules/volume-placement/volume-placement.server";
import type { Database } from "#/server/database.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const organizationId = "00000000-0000-4000-8000-000000000f01";
const volumeId = "vol1";
const environment = { project: "shop", environment: "production" };
const web = (name: string) => name.padEnd(32, "0") as MachineId;

describe("volume placement", () => {
  let harness: PostgresTestHarness;
  let inFlight: boolean;

  const run = <A, E>(operation: Effect.Effect<A, E, Database | CloudStore>) => harness.runEffect(operation.pipe(
    Effect.provideService(CloudStore, {
      open: Effect.succeed(asTestDouble<ConfigStore>()({
        read: async (_: string, query: { query: string }) => {
          if (query.query === "namespaces") return { namespaces: [{ namespace: "shop-production", project: "shop", environment: "production" }] };
          if (query.query === "deployment") return { in_flight: inFlight };
          return { environment: { id: "env-1", project: "shop", name: "production", revision: 1 }, volumes: [{ id: volumeId, name: "data", storage: { kind: "provisioned", maximumBytes: 1 } }] };
        },
      })),
    }),
  ));
  const stage = (primary: string, mirrors: string[] = []) =>
    run(stagePlacement(organizationId, { environment, volumeId, staged: { primary: web(primary), mirrors: mirrors.map(web), deletes: [] }, mirrors: [web("web-2")] }));

  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });
  beforeEach(async () => {
    inFlight = true;
    await harness.pool.query(`
      truncate table organization cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
    `);
  });

  it("records the mirrors seen at the first stage, and keeps them when the user stages again", async () => {
    await stage("web-2");
    await stage("web-3");
    expect(await run(listPlacements(organizationId))).toEqual([{
      volumeId, environmentId: "env-1", mirrors: [web("web-2")], staged: { primary: web("web-3"), mirrors: [], deletes: [] }, deploymentId: null,
    }]);
  });

  it("hands the staged placement to the Deploy, and refuses Discard and new edits while it runs", async () => {
    await stage("web-2");
    await run(stampPlacements(organizationId, "env-1", "dep-1"));
    expect((await run(listPlacements(organizationId)))[0]?.deploymentId).toBe("dep-1");
    expect(await run(discardPlacement(organizationId, volumeId))).toMatchObject({ ok: false, refusal: { code: "conflict" } });
    expect(await stage("web-3")).toMatchObject({ ok: false });
    expect((await run(listPlacements(organizationId)))[0]?.staged?.primary).toBe(web("web-2"));
  });

  it("discards once the Deploy has ended", async () => {
    await stage("web-2");
    await run(stampPlacements(organizationId, "env-1", "dep-1"));
    inFlight = false;
    expect(await run(discardPlacement(organizationId, volumeId))).toMatchObject({ ok: true, placement: { staged: null, deploymentId: null } });
  });

  it("stamps only Volumes with something staged in that Environment", async () => {
    await stage("web-2");
    await run(discardPlacement(organizationId, volumeId));
    await run(stampPlacements(organizationId, "env-1", "dep-1"));
    expect((await run(listPlacements(organizationId)))[0]?.deploymentId).toBeNull();
  });
});
