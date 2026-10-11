import type { ConfigStore, CopyObservation, MachineId, VolumeKind } from "@ployz/sdk";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import {
  advancePlacement, type MemberInspector, refreshMirrors, releasePlacementClaims, stageOffServer, unlessPlacementRefused,
} from "#/modules/volume-placement/volume-placement.deploy.server";
import { listPlacements, stagePlacement, stampPlacements } from "#/modules/volume-placement/volume-placement.server";
import type { Member } from "#/modules/volume-run/volume-run";
import { member } from "#/modules/volume-run/volume-run.test-fixture";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { Database } from "#/server/database.server";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const organizationId = "00000000-0000-4000-8000-000000000f02";
const volumeId = "vol1";
const environment = { project: "shop", environment: "production" };
const web = (name: string) => name.padEnd(32, "0") as MachineId;
const inngest = new Inngest({ id: "volume-placement-test" });
const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

describe("deploying a Volume placement", () => {
  let harness: PostgresTestHarness;
  let status: string;
  let members: Member[];
  let restoredStorage: VolumeKind;
  const inspect: MemberInspector = () => Effect.succeed(members);

  const run = <A, E>(operation: Effect.Effect<A, E, Database | SecretEncryption | CloudStore | InngestClient | OrganizationRuntime>) => harness.runEffect(operation.pipe(
    Effect.provideService(InngestClient, inngest),
    Effect.provideService(OrganizationRuntime, { cancel: () => Effect.void, open: () => Effect.succeed({ status: "no_connection" as const }) }),
    Effect.provideService(CloudStore, {
      open: Effect.succeed(asTestDouble<ConfigStore>()({
        read: async (_: string, query: { query: string }) => {
          if (query.query === "namespaces") return { namespaces: [{ namespace: "shop-production", project: "shop", environment: "production" }] };
          if (query.query === "deployment") return { status, in_flight: status === "queued", environment: { id: "env-1", project: "shop", name: "production" } };
          return {
            environment: { id: "env-1", project: "shop", name: "production", revision: 1 },
            volumes: [{ id: volumeId, name: "data", storage: { kind: "provisioned", maximumBytes: 1 } }, { id: "vol2", name: "data-1430", storage: restoredStorage }],
          };
        },
      })),
    }),
  ));
  const placement = async () => (await run(listPlacements(organizationId)))[0];
  const stageMove = async (to: string, mirrors: string[]) => {
    await run(stagePlacement(organizationId, { environment, volumeId, staged: { primary: web(to), mirrors: [], deletes: [] }, mirrors: mirrors.map(web) }));
    await run(stampPlacements(organizationId, "env-1", "dep-1"));
  };
  const endRun = (id: string, state: string, message: string | null = null) =>
    harness.pool.query(`update volume_run set state = $2, message = $3, finished_at = now() where id = $1`, [id, state, message]);

  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });
  beforeEach(async () => {
    status = "queued";
    restoredStorage = { kind: "provisioned", maximumBytes: 1 };
    send.mockClear();
    await harness.pool.query(`
      truncate table organization cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
    `);
  });

  it("promotes the mirror, then settles the Volume with no mirror and nothing staged", async () => {
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    await stageMove("web-2", ["web-2"]);
    const started = await run(advancePlacement(organizationId, "dep-1", null, inspect));
    expect(started).toMatchObject({ kind: "wait", wait: { volumeId, mirrors: [] } });
    if (started.kind !== "wait") throw new Error("expected a run");
    const { rows } = await harness.pool.query(`select kind, args, requested_by_user_id from volume_run where id = $1`, [started.wait.runId]);
    expect(rows).toEqual([{ kind: "move", args: { to: "web-2" }, requested_by_user_id: null }]);
    expect(await run(advancePlacement(organizationId, "dep-1", started.wait, inspect))).toEqual(started);

    await endRun(started.wait.runId, "done");
    members = [member("web-2", "writer"), member("web-1", "mirror")];
    expect(await run(advancePlacement(organizationId, "dep-1", started.wait, inspect))).toEqual({ kind: "done" });
    expect(await placement()).toMatchObject({ mirrors: [], staged: null, deploymentId: null });
  });

  it("fails the Deploy when a run fails, and the Volume stays staged for the next one", async () => {
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    await stageMove("web-2", ["web-2"]);
    const started = await run(advancePlacement(organizationId, "dep-1", null, inspect));
    if (started.kind !== "wait") throw new Error("expected a run");
    await endRun(started.wait.runId, "failed", "web-2 did not answer");
    expect(await run(advancePlacement(organizationId, "dep-1", started.wait, inspect))).toEqual({ kind: "failed", message: "data: web-2 did not answer" });
    expect(await placement()).toMatchObject({ mirrors: [web("web-2")], staged: { primary: web("web-2") }, deploymentId: null });
  });

  it("stops between runs once the Deploy is cancelled, leaving the Volume staged", async () => {
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    await stageMove("web-2", ["web-2"]);
    status = "cancelled";
    expect(await run(advancePlacement(organizationId, "dep-1", null, inspect))).toEqual({ kind: "stopped" });
    expect(await placement()).toMatchObject({ staged: { primary: web("web-2") }, deploymentId: null });
    expect(send).not.toHaveBeenCalled();
  });

  it("returns an ended Deploy's Volumes to staged, and leaves one still in flight its claim", async () => {
    await stageMove("web-2", ["web-2"]);
    await run(releasePlacementClaims(organizationId, "dep-1"));
    expect(await placement()).toMatchObject({ staged: { primary: web("web-2") }, deploymentId: "dep-1" });
    status = "failed";
    await run(releasePlacementClaims(organizationId, "dep-1"));
    expect(await placement()).toMatchObject({ staged: { primary: web("web-2") }, deploymentId: null });
  });

  it("waits for a run it didn't start, then plans again", async () => {
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    const other = await harness.pool.query(`
      insert into volume_run (organization_id, environment_id, volume_id, volume_name, docker_volume, kind, args, state)
      values ($1, 'env-1', $2, 'data', 'shop-production_vol-vol1', 'sync', '{"full":false}', 'running') returning id`, [organizationId, volumeId]);
    await stageMove("web-2", ["web-2"]);
    const waiting = await run(advancePlacement(organizationId, "dep-1", null, inspect));
    expect(waiting).toEqual({ kind: "wait", wait: { runId: other.rows[0].id, volumeId, mirrors: null } });
    await endRun(other.rows[0].id, "failed");
    expect(await run(advancePlacement(organizationId, "dep-1", waiting.kind === "wait" ? waiting.wait : null, inspect)))
      .toMatchObject({ kind: "wait", wait: { mirrors: [] } });
  });

  it("restores before it moves the source, running the copy on the source", async () => {
    const restored = [member("web-1", "writer")];
    const source = [member("web-1", "writer"), member("web-2", "mirror")];
    let landed = false;
    const byVolume: MemberInspector = (_, _environment, id) => Effect.succeed(id === "vol2" ? (landed ? restored : []) : source);
    await stageMove("web-2", ["web-2"]);
    await run(stagePlacement(organizationId, { environment, volumeId: "vol2", mirrors: [], staged: {
      primary: web("web-1"), mirrors: [], deletes: [],
      seed: { sourceVolumeId: volumeId, sourceName: "data", dockerVolume: "shop-production_vol-vol2", point: { guid: "11", name: "ployz-1", at: 1_790_000_000 } },
    } }));
    await run(stampPlacements(organizationId, "env-1", "dep-1"));

    const seeding = await run(advancePlacement(organizationId, "dep-1", null, byVolume));
    expect(seeding).toMatchObject({ kind: "wait", wait: { volumeId: "vol2", mirrors: [] } });
    if (seeding.kind !== "wait") throw new Error("expected a run");
    const { rows } = await harness.pool.query(`select volume_id, kind, args from volume_run where id = $1`, [seeding.wait.runId]);
    expect(rows).toEqual([{
      volume_id: volumeId, kind: "seed",
      args: { into: { volumeId: "vol2", dockerVolume: "shop-production_vol-vol2", name: "data-1430" }, guid: "11", name: "ployz-1" },
    }]);

    await endRun(seeding.wait.runId, "done");
    landed = true;
    const moving = await run(advancePlacement(organizationId, "dep-1", seeding.wait, byVolume));
    expect(moving).toMatchObject({ kind: "wait", wait: { volumeId } });
    if (moving.kind !== "wait") throw new Error("expected a run");
    expect((await harness.pool.query(`select kind from volume_run where id = $1`, [moving.wait.runId])).rows).toEqual([{ kind: "move" }]);
    const rowOf = async (id: string) => (await run(listPlacements(organizationId))).find((row) => row.volumeId === id);
    expect(await rowOf("vol2")).toMatchObject({ mirrors: [], staged: null, deploymentId: null });
  });

  it("fails with the upgrade message once a Server with storage runs an older daemon", async () => {
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    await stageMove("web-2", ["web-2"]);

    expect(await run(advancePlacement(organizationId, "dep-1", null, inspect, () => Effect.succeed(["db-old"])))).toEqual({
      kind: "failed", message: expect.stringContaining("db-old"),
    });
    expect((await harness.pool.query(`select id from volume_run`)).rows).toEqual([]);
    expect(await placement()).toMatchObject({ staged: { primary: web("web-2") }, deploymentId: null });
  });

  it("keeps a restore staged when its Volume isn't in the Store yet", async () => {
    const seed = { sourceVolumeId: volumeId, sourceName: "data", dockerVolume: "shop-production_vol-vol3", point: { guid: "11", name: "ployz-1", at: 1_790_000_000 } };
    await harness.pool.query(
      `insert into volume_placement (volume_id, organization_id, environment_id, mirrors, staged) values ('vol3', $1, 'env-1', '[]', $2)`,
      [organizationId, JSON.stringify({ primary: web("web-1"), mirrors: [], deletes: [], seed })],
    );
    await run(stampPlacements(organizationId, "env-1", "dep-1"));

    expect(await run(advancePlacement(organizationId, "dep-1", null, inspect))).toEqual({ kind: "done" });
    expect(await placement()).toMatchObject({ volumeId: "vol3", staged: { seed }, deploymentId: null });
  });

  it("refuses to seed a restored Volume whose storage no longer matches its source", async () => {
    members = [member("web-1", "writer")];
    await run(stagePlacement(organizationId, { environment, volumeId: "vol2", mirrors: [], staged: {
      primary: web("web-1"), mirrors: [], deletes: [],
      seed: { sourceVolumeId: volumeId, sourceName: "data", dockerVolume: "shop-production_vol-vol2", point: { guid: "11", name: "ployz-1", at: 1_790_000_000 } },
    } }));
    await run(stampPlacements(organizationId, "env-1", "dep-1"));
    restoredStorage = { kind: "docker" };

    expect(await run(advancePlacement(organizationId, "dep-1", null, (_, _environment, id) => Effect.succeed(id === "vol2" ? [] : members)))).toEqual({
      kind: "failed",
      message: "data-1430: data-1430's storage no longer matches data's, so the restore can't land on it. Discard the restore and restore again.",
    });
    expect((await harness.pool.query(`select id from volume_run`)).rows).toEqual([]);
  });

  describe("refreshing mirrors", () => {
    const copy = (server: string, role: "writer" | "slot") => ({ machine_id: web(server), name: "shop-production_vol-vol1", role });
    const refresh = (copies: CopyObservation["copies"], blockers: string[] = []) =>
      run(refreshMirrors(organizationId, null, () => Effect.succeed(copies), () => Effect.succeed(blockers)));
    const requestedKinds = async () => (await harness.pool.query(`select kind from volume_run`)).rows.map((row) => row.kind);

    it("syncs a copy the dashboard has no record for, as the CLI made it a mirror", async () => {
      expect(await refresh([copy("web-1", "writer"), copy("web-2", "slot")])).toHaveLength(1);
      expect(await requestedKinds()).toEqual(["sync"]);
    });

    it("never refreshes a detached copy", async () => {
      await run(stagePlacement(organizationId, { environment, volumeId, staged: null, mirrors: [] }));
      expect(await refresh([copy("web-2", "writer"), copy("web-1", "slot")])).toEqual([]);
    });

    it("syncs when an old primary came back, so the run demotes it", async () => {
      await run(stagePlacement(organizationId, { environment, volumeId, staged: null, mirrors: [] }));
      expect(await refresh([copy("web-2", "writer"), copy("web-1", "writer")])).toHaveLength(1);
    });

    it("leaves a Volume a Deploy is moving to that Deploy", async () => {
      await stageMove("web-2", ["web-2"]);
      expect(await refresh([copy("web-1", "writer"), copy("web-2", "slot")])).toEqual([]);
    });

    it("skips an organization while a Server with storage runs an older daemon", async () => {
      expect(await refresh([copy("web-1", "writer"), copy("web-2", "slot")], ["db-old"])).toEqual([]);
      expect(await requestedKinds()).toEqual([]);
    });
  });

  describe("gating a staged placement on every Server with storage", () => {
    const stage = (read: () => Effect.Effect<readonly string[], string>) => run(unlessPlacementRefused(organizationId,
      stagePlacement(organizationId, { environment, volumeId, staged: { primary: web("web-2"), mirrors: [], deletes: [] }, mirrors: [] }), read));

    it("refuses while one runs an older daemon, and stages nothing", async () => {
      expect(await stage(() => Effect.succeed(["db-old"]))).toEqual({ ok: false, refusal: {
        code: "upgrade_required", message: "Every Server with storage must run Ployz 0.2.5 or later. Upgrade db-old first.", details: { servers: ["db-old"] },
      } });
      expect(await run(listPlacements(organizationId))).toEqual([]);
    });

    it("refuses while the Servers can't say", async () => {
      expect(await stage(() => Effect.fail("timeout"))).toMatchObject({ ok: false, refusal: { code: "unanswered" } });
    });

    it("stages once every one runs the verbs", async () => {
      expect(await stage(() => Effect.succeed([]))).toMatchObject({ ok: true, placement: { staged: { primary: web("web-2") } } });
    });
  });

  describe("draining a Server", () => {
    const copy = (server: string, role: "writer" | "slot") => ({ machine_id: web(server), name: "shop-production_vol-vol1", role });
    const drain = (copies: CopyObservation["copies"], accepting: string[]) =>
      run(stageOffServer(organizationId, web("web-1"), accepting.map(web), () => Effect.succeed(copies), inspect));

    it("stages the primary on the drained Server to its mirror", async () => {
      members = [member("web-1", "writer"), member("web-2", "mirror")];
      expect(await drain([copy("web-1", "writer"), copy("web-2", "slot")], ["web-2"])).toEqual({ stuck: [] });
      expect(await placement()).toMatchObject({ mirrors: [web("web-2")], staged: { primary: web("web-2"), mirrors: [] }, deploymentId: null });
    });

    it("names a Volume nowhere takes, and leaves a primary on another Server alone", async () => {
      members = [member("web-1", "writer")];
      expect(await drain([copy("web-1", "writer")], [])).toEqual({ stuck: ["data"] });
      expect(await drain([copy("web-2", "writer")], [])).toEqual({ stuck: [] });
      expect(await run(listPlacements(organizationId))).toEqual([]);
    });
  });
});
