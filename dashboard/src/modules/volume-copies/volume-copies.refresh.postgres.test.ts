import type { ConfigStore, MachineId } from "@ployz/sdk";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { lastMirrorRefresh, refreshMirror } from "#/modules/volume-copies/volume-copies.refresh.server";
import { stagePlacement, stampPlacements } from "#/modules/volume-placement/volume-placement.server";
import { requestVolumeRun } from "#/modules/volume-run/volume-run.server";
import type { Database } from "#/server/database.server";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const organizationId = "00000000-0000-4000-8000-000000000f04";
const caller = { userId: null, organizationId };
const volumeId = "vol1";
const environment = { project: "shop", environment: "production" };
const web = (name: string) => name.padEnd(32, "0") as MachineId;
const inngest = new Inngest({ id: "volume-copies-refresh-test" });
vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

describe("Refresh now", () => {
  let harness: PostgresTestHarness;
  let blockers: string[];

  const run = <A, E>(operation: Effect.Effect<A, E, Database | SecretEncryption | CloudStore | InngestClient | OrganizationRuntime>) => harness.runEffect(operation.pipe(
    Effect.provideService(InngestClient, inngest),
    Effect.provideService(OrganizationRuntime, { cancel: () => Effect.void, open: () => Effect.succeed({ status: "no_connection" as const }) }),
    Effect.provideService(CloudStore, {
      open: Effect.succeed(asTestDouble<ConfigStore>()({
        read: async (_: string, query: { query: string }) => {
          if (query.query === "namespaces") return { namespaces: [{ namespace: "shop-production", project: "shop", environment: "production" }] };
          if (query.query === "deployment") return { status: "queued", in_flight: true, environment: { id: "env-1", project: "shop", name: "production" } };
          return {
            environment: { id: "env-1", project: "shop", name: "production", revision: 1 },
            volumes: [{ id: volumeId, name: "data", storage: { kind: "provisioned", maximumBytes: 1 } }],
          };
        },
      })),
    }),
  ));
  const refresh = () => run(refreshMirror(caller, environment, volumeId, () => Effect.succeed(blockers)));
  const runs = async () => (await harness.pool.query(`select kind, state from volume_run order by created_at`)).rows;
  const endRun = (id: string, state: string, finishedAt = "now()") =>
    harness.pool.query(`update volume_run set state = $2, finished_at = ${finishedAt} where id = $1`, [id, state]);

  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });
  beforeEach(async () => {
    blockers = [];
    await harness.pool.query(`
      truncate table organization cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
    `);
  });

  it("requests one mirror round at once", async () => {
    expect(await refresh()).toMatchObject({ ok: true, run: { kind: "sync", state: "requested", args: { full: false } } });
    expect(await runs()).toEqual([{ kind: "sync", state: "requested" }]);
  });

  it("answers busy while a round runs, and queues no second", async () => {
    await refresh();
    expect(await refresh()).toEqual({ ok: false, refusal: { code: "conflict", message: "Already refreshing", details: null } });
    expect(await runs()).toHaveLength(1);
  });

  it("answers busy with what else runs on the Volume", async () => {
    await run(requestVolumeRun(caller, { volumeId, environment, kind: "move", args: { to: "web-2" } }));
    expect(await refresh()).toEqual({ ok: false, refusal: { code: "conflict", message: "Busy. Moving to web-2.", details: null } });
    expect(await runs()).toEqual([{ kind: "move", state: "requested" }]);
  });

  it("leaves a Volume a Deploy is moving to that Deploy", async () => {
    await run(stagePlacement(organizationId, { environment, volumeId, staged: { primary: web("web-2"), mirrors: [], deletes: [] }, mirrors: [web("web-2")] }));
    await run(stampPlacements(organizationId, "env-1", "dep-1"));
    expect(await refresh()).toEqual({ ok: false, refusal: { code: "conflict", message: "A deploy is moving this Volume", details: null } });
    expect(await runs()).toEqual([]);
  });

  it("refuses while a Server with storage runs an older daemon", async () => {
    blockers = ["db-old"];
    expect(await refresh()).toMatchObject({ ok: false, refusal: { code: "upgrade_required", details: { servers: ["db-old"] } } });
    expect(await runs()).toEqual([]);
  });

  it("runs again once the last round ended", async () => {
    const first = await refresh();
    if (!first.ok) throw new Error("expected a run");
    await endRun(first.run.id, "failed");
    expect(await refresh()).toMatchObject({ ok: true });
  });

  it("reads the newest round that ended, failed or not, and the newest that ended done", async () => {
    expect(await run(lastMirrorRefresh(organizationId, volumeId))).toEqual({ ended: null, done: null });
    const first = await refresh();
    if (!first.ok) throw new Error("expected a run");
    await endRun(first.run.id, "done", "now() - interval '2 hours'");
    const second = await refresh();
    if (!second.ok) throw new Error("expected a run");
    expect(await run(lastMirrorRefresh(organizationId, volumeId))).toMatchObject({ ended: { id: first.run.id }, done: { id: first.run.id } });
    await endRun(second.run.id, "lost");
    expect(await run(lastMirrorRefresh(organizationId, volumeId))).toMatchObject({ ended: { id: second.run.id, state: "lost" }, done: { id: first.run.id } });
  });
});
