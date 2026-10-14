import type { ConfigCommand, ConfigCommitted, ConfigStore, CopyObservation, MachineId, VolumeKind, WantedCopy } from "@ployz/sdk";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import {
  advancePlacement, type MemberInspector, refreshMirrors, releasePlacementClaims, stageOffServer, unlessPlacementRefused,
} from "#/modules/volume-placement/volume-placement.deploy.server";
import { listPlacements, stagePlacement, stampPlacements, writeAdmitted } from "#/modules/volume-placement/volume-placement.server";
import { placementTargets } from "#/modules/volume-placement/volume-placement";
import type { Member } from "#/modules/volume-run/volume-run";
import type { CopyAsk } from "#/modules/volume-placement/mirror-sync.server";
import type { CopyAnswer } from "#/modules/volume-run/volume-run.server";
import { COPIES, member } from "#/modules/volume-run/volume-run.test-fixture";
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
/** Every Server answers, each on a daemon that runs placement. */
const answered = () => Effect.succeed([]);

describe("deploying a Volume placement", () => {
  let harness: PostgresTestHarness;
  let status: string;
  /** A Deployment's own status, where it differs from `status`. */
  const statuses = new Map<string, string>();
  let members: Member[];
  let restoredStorage: VolumeKind;
  const inspect: MemberInspector = () => Effect.succeed(members);

  const run = <A, E>(operation: Effect.Effect<A, E, Database | SecretEncryption | CloudStore | InngestClient | OrganizationRuntime>) => harness.runEffect(operation.pipe(
    Effect.provideService(InngestClient, inngest),
    Effect.provideService(OrganizationRuntime, { cancel: () => Effect.void, open: () => Effect.succeed({ status: "no_connection" as const }) }),
    Effect.provideService(CloudStore, {
      open: Effect.succeed(asTestDouble<ConfigStore>()({
        read: async (_: string, query: { query: string; id?: string }) => {
          if (query.query === "namespaces") return { namespaces: [{ namespace: "shop-production", project: "shop", environment: "production" }] };
          if (query.query === "deployment") {
            const state = statuses.get(query.id ?? "") ?? status;
            return { status: state, in_flight: state === "queued", environment: { id: "env-1", project: "shop", name: "production" }, nodes: [] };
          }
          return {
            environment: { id: "env-1", project: "shop", name: "production", revision: 1 },
            volumes: [{ id: volumeId, name: "data", storage: { kind: "provisioned", maximumBytes: 1 } }, { id: "vol2", name: "data-1430", storage: restoredStorage }],
          };
        },
      })),
    }),
  ));
  const placement = async () => (await run(listPlacements(organizationId)))[0];
  // A restore request still writing to the Store holds this lock until its write commits or fails.
  const whileStoreWriting = async <A>(body: () => Promise<A>) => {
    const stager = await harness.pool.connect();
    try {
      await stager.query("begin");
      await stager.query("select pg_advisory_xact_lock(hashtext($1))", ["volume-environment:env-1"]);
      return await body();
    } finally {
      await stager.query("rollback");
      stager.release();
    }
  };
  const stageMove = async (to: string, mirrors: string[]) => {
    await run(stagePlacement(organizationId, { environment, volumeId, staged: { primary: web(to), mirrors: [], deletes: [] }, mirrors: mirrors.map(web) }));
    await run(stampPlacements(organizationId, [volumeId], "dep-1"));
  };
  const endRun = (id: string, state: string, message: string | null = null) =>
    harness.pool.query(`update volume_run set state = $2, message = $3, finished_at = now() where id = $1`, [id, state, message]);

  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });
  beforeEach(async () => {
    status = "queued";
    statuses.clear();
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
    const started = await run(advancePlacement(organizationId, "dep-1", null, inspect, answered));
    expect(started).toMatchObject({ kind: "wait", wait: { volumeId, step: { mirrors: [] } } });
    if (started.kind !== "wait") throw new Error("expected a run");
    const { rows } = await harness.pool.query(`select kind, args, requested_by_user_id from volume_run where id = $1`, [started.wait.runId]);
    expect(rows).toEqual([{ kind: "move", args: { to: "web-2" }, requested_by_user_id: null }]);
    expect(await run(advancePlacement(organizationId, "dep-1", started.wait, inspect, answered))).toEqual(started);

    await endRun(started.wait.runId, "done");
    members = [member("web-2", "writer"), member("web-1", "mirror")];
    expect(await run(advancePlacement(organizationId, "dep-1", started.wait, inspect, answered))).toEqual({ kind: "done" });
    expect(await placement()).toMatchObject({ mirrors: [], staged: null, deploymentId: null });
  });

  it("fails the Deploy when a run fails, dropping the failed change and returning the ones it never reached", async () => {
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    await stageMove("web-2", ["web-2"]);
    const started = await run(advancePlacement(organizationId, "dep-1", null, inspect, answered));
    if (started.kind !== "wait") throw new Error("expected a run");
    const unreached = { primary: web("web-1"), mirrors: [web("web-2")], deletes: [] };
    await run(stagePlacement(organizationId, { environment, volumeId: "vol2", staged: unreached, mirrors: [] }));
    await run(stampPlacements(organizationId, [volumeId, "vol2"], "dep-1"));

    await endRun(started.wait.runId, "failed", "web-2 did not answer");
    expect(await run(advancePlacement(organizationId, "dep-1", started.wait, inspect, answered))).toEqual({ kind: "failed", message: "data: web-2 did not answer" });
    const rows = await run(listPlacements(organizationId));
    expect(rows.find((row) => row.volumeId === volumeId)).toMatchObject({ mirrors: [web("web-2")], staged: null, deploymentId: null });
    // Exactly what the user staged comes back, so editing a returned change sends the Store a placement it accepts.
    expect(rows.find((row) => row.volumeId === "vol2")).toEqual(expect.objectContaining({ staged: unreached, unreached: true, deploymentId: null }));

    await run(stampPlacements(organizationId, ["vol2"], "dep-2"));
    expect((await run(listPlacements(organizationId))).find((row) => row.volumeId === "vol2")).toEqual(expect.objectContaining({ staged: unreached, unreached: false, deploymentId: "dep-2" }));
  });

  it("drops only the change that failed, returning the Volume's other changes", async () => {
    members = [member("web-1", "writer"), member("web-2", "mirror"), member("web-3", "mirror")];
    await run(stagePlacement(organizationId, { environment, volumeId, staged: { primary: web("web-2"), mirrors: [], deletes: [web("web-3")] }, mirrors: [web("web-2")] }));
    await run(stampPlacements(organizationId, [volumeId], "dep-1"));
    const started = await run(advancePlacement(organizationId, "dep-1", null, inspect, answered));
    if (started.kind !== "wait") throw new Error("expected a run");
    expect((await harness.pool.query(`select kind from volume_run where id = $1`, [started.wait.runId])).rows).toEqual([{ kind: "delete_mirror" }]);

    await endRun(started.wait.runId, "failed", "web-3 is busy");
    expect(await run(advancePlacement(organizationId, "dep-1", started.wait, inspect, answered))).toEqual({ kind: "failed", message: "data: web-3 is busy" });
    expect(await placement()).toEqual(expect.objectContaining({
      mirrors: [web("web-2")], staged: { primary: web("web-2"), mirrors: [], deletes: [] }, unreached: true, deploymentId: null,
    }));
  });

  it("settles a mirror whose removal landed despite the failed run, so Add copy offers its Server again", async () => {
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    await run(stagePlacement(organizationId, { environment, volumeId, staged: { primary: web("web-1"), mirrors: [], deletes: [] }, mirrors: [web("web-2")] }));
    await run(stampPlacements(organizationId, [volumeId], "dep-1"));
    const started = await run(advancePlacement(organizationId, "dep-1", null, inspect, answered));
    if (started.kind !== "wait") throw new Error("expected a run");

    await endRun(started.wait.runId, "failed", "web-2 lost the reply");
    members = [member("web-1", "writer"), member("web-2", "empty")];
    expect(await run(advancePlacement(organizationId, "dep-1", started.wait, inspect, answered))).toEqual({ kind: "failed", message: "data: web-2 lost the reply" });
    const row = await placement();
    expect(row).toMatchObject({ mirrors: [], staged: null, deploymentId: null });
    if (row === undefined) throw new Error("expected the row");
    expect(placementTargets({ id: volumeId, name: "data" }, members, row, [web("web-2")]).mirror.map((server) => server.name)).toEqual(["web-2"]);
  });

  it("settles a landed mirror removal while another change stays staged", async () => {
    members = [member("web-1", "writer"), member("web-2", "mirror"), member("web-3", "empty")];
    const staged = { primary: web("web-1"), mirrors: [web("web-3")], deletes: [] };
    await run(stagePlacement(organizationId, { environment, volumeId, staged, mirrors: [web("web-2")] }));
    await run(stampPlacements(organizationId, [volumeId], "dep-1"));
    const started = await run(advancePlacement(organizationId, "dep-1", null, inspect, answered));
    if (started.kind !== "wait") throw new Error("expected a run");
    expect((await harness.pool.query(`select args ->> 'slot' as slot from volume_run where id = $1`, [started.wait.runId])).rows).toEqual([{ slot: "web-2" }]);

    await endRun(started.wait.runId, "failed", "web-2 lost the reply");
    members = [member("web-1", "writer"), member("web-2", "empty"), member("web-3", "empty")];
    await run(advancePlacement(organizationId, "dep-1", started.wait, inspect, answered));
    expect(await placement()).toMatchObject({ mirrors: [], staged, deploymentId: null });
  });

  it("keeps a move that failed after handover reached, and marks only the Volumes the Deploy never got to", async () => {
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    await stageMove("web-2", ["web-2"]);
    const started = await run(advancePlacement(organizationId, "dep-1", null, inspect, answered));
    if (started.kind !== "wait") throw new Error("expected a run");
    const untouched = { primary: web("web-1"), mirrors: [web("web-2")], deletes: [] };
    await run(stagePlacement(organizationId, { environment, volumeId: "vol2", staged: untouched, mirrors: [] }));
    await run(stampPlacements(organizationId, ["vol2"], "dep-1"));

    await endRun(started.wait.runId, "failed", "web-2 did not start");
    members = [member("web-2", "writer"), member("web-1", "mirror")];
    expect(await run(advancePlacement(organizationId, "dep-1", started.wait, inspect, answered))).toEqual({ kind: "failed", message: "data: web-2 did not start" });
    const rows = await run(listPlacements(organizationId));
    expect(rows.find((row) => row.volumeId === volumeId)).toEqual(expect.objectContaining({ staged: { primary: web("web-2"), mirrors: [], deletes: [] }, unreached: false, deploymentId: null }));
    expect(rows.find((row) => row.volumeId === "vol2")).toEqual(expect.objectContaining({ staged: untouched, unreached: true, deploymentId: null }));
  });

  it("keeps a restore whose copy failed claimed, for the Store failure to take back", async () => {
    const source = [member("web-1", "writer"), member("web-2", "mirror")];
    const byVolume: MemberInspector = (_, _environment, id) => Effect.succeed(id === "vol2" ? [] : source);
    await stageMove("web-2", ["web-2"]);
    await run(stagePlacement(organizationId, { environment, volumeId: "vol2", mirrors: [], staged: {
      primary: web("web-1"), mirrors: [], deletes: [],
      seed: { sourceVolumeId: volumeId, sourceName: "data", dockerVolume: "shop-production_vol-vol2", point: { guid: "11", name: "ployz-1", at: 1_790_000_000 } },
    } }));
    await run(stampPlacements(organizationId, [volumeId, "vol2"], "dep-1"));
    const seeding = await run(advancePlacement(organizationId, "dep-1", null, byVolume, answered));
    if (seeding.kind !== "wait") throw new Error("expected a run");

    await endRun(seeding.wait.runId, "failed", "no space left");
    expect(await run(advancePlacement(organizationId, "dep-1", seeding.wait, byVolume, answered)))
      .toEqual({ kind: "failed", message: "data: no space left", restore: { volumeId: "vol2", environment } });
    expect((await run(listPlacements(organizationId))).map(({ volumeId: id, deploymentId }) => ({ id, deploymentId })))
      .toEqual(expect.arrayContaining([{ id: volumeId, deploymentId: "dep-1" }, { id: "vol2", deploymentId: "dep-1" }]));
  });

  it("leaves a restore staged, and records no failure, when its seed was taken over rather than failed", async () => {
    const source = [member("web-1", "writer"), member("web-2", "mirror")];
    const byVolume: MemberInspector = (_, _environment, id) => Effect.succeed(id === "vol2" ? [] : source);
    await stageMove("web-2", ["web-2"]);
    await run(stagePlacement(organizationId, { environment, volumeId: "vol2", mirrors: [], staged: {
      primary: web("web-1"), mirrors: [], deletes: [],
      seed: { sourceVolumeId: volumeId, sourceName: "data", dockerVolume: "shop-production_vol-vol2", point: { guid: "11", name: "ployz-1", at: 1_790_000_000 } },
    } }));
    await run(stampPlacements(organizationId, [volumeId, "vol2"], "dep-1"));
    const seeding = await run(advancePlacement(organizationId, "dep-1", null, byVolume, answered));
    if (seeding.kind !== "wait") throw new Error("expected a run");

    await endRun(seeding.wait.runId, "yielded", "a Deploy took this run over");
    expect(await run(advancePlacement(organizationId, "dep-1", seeding.wait, byVolume, answered)))
      .toEqual({ kind: "failed", message: "data: a Deploy took this run over" });
    expect(await failedRestores()).toBe(0);
    expect((await run(listPlacements(organizationId))).find((row) => row.volumeId === "vol2"))
      .toMatchObject({ staged: { seed: { sourceVolumeId: volumeId } }, deploymentId: null });
  });

  it("takes over a restore a superseded Deploy stamped, and runs it", async () => {
    const source = [member("web-1", "writer"), member("web-2", "mirror")];
    const byVolume: MemberInspector = (_, _environment, id) => Effect.succeed(id === "vol2" ? [] : source);
    await run(stagePlacement(organizationId, { environment, volumeId: "vol2", mirrors: [], staged: {
      primary: web("web-1"), mirrors: [], deletes: [],
      seed: { sourceVolumeId: volumeId, sourceName: "data", dockerVolume: "shop-production_vol-vol2", point: { guid: "11", name: "ployz-1", at: 1_790_000_000 } },
    } }));
    await run(stampPlacements(organizationId, ["vol2"], "dep-1"));
    statuses.set("dep-1", "superseded");

    const admit = (accept: string[], version: string): ConfigCommand =>
      ({ command: "admit", admit: "deploy", id: "dep-2", environment, services: [], version, accept_volume_loss: accept });
    const write = () => Effect.succeed(asTestDouble<ConfigCommitted>()({ written: "deployment", id: "dep-2", environment_id: "env-1", status: "admitted", checks: [] }));
    const refused = await run(writeAdmitted(organizationId, admit([], "v1"), write, byVolume).pipe(
      Effect.flatMap(() => Effect.die("expected a refusal")),
      Effect.catchTag("StoreRefused", (error) => Effect.succeed(error.refusal)),
    ));
    const { accept, version } = refused.details as { accept: string[]; version: string };
    await run(writeAdmitted(organizationId, admit(accept, version), write, byVolume));
    expect((await run(listPlacements(organizationId))).find((row) => row.volumeId === "vol2")?.deploymentId).toBe("dep-2");

    const seeding = await run(advancePlacement(organizationId, "dep-2", null, byVolume, answered));
    expect(seeding).toMatchObject({ kind: "wait", wait: { volumeId: "vol2", step: { change: { kind: "seed" } } } });
  });

  it("stops between runs once the Deploy is cancelled, leaving the Volume staged", async () => {
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    await stageMove("web-2", ["web-2"]);
    status = "cancelled";
    expect(await run(advancePlacement(organizationId, "dep-1", null, inspect, answered))).toEqual({ kind: "stopped" });
    expect(await placement()).toMatchObject({ staged: { primary: web("web-2") }, deploymentId: null });
    expect(send).not.toHaveBeenCalled();
  });

  it("returns an ended Deploy's Volumes to staged, and leaves one still in flight its claim", async () => {
    await stageMove("web-2", ["web-2"]);
    await run(releasePlacementClaims(organizationId, "dep-1"));
    expect(await placement()).toMatchObject({ staged: { primary: web("web-2") }, deploymentId: "dep-1" });
    status = "failed";
    await run(releasePlacementClaims(organizationId, "dep-1"));
    expect(await placement()).toMatchObject({ staged: { primary: web("web-2") }, unreached: true, deploymentId: null });
  });

  describe("once a cancelled Deploy gave its claim to the next one mid-step", () => {
    const handOver: MemberInspector = () => Effect.promise(async () => {
      status = "cancelled";
      await run(releasePlacementClaims(organizationId, "dep-1"));
      status = "queued";
      await run(stampPlacements(organizationId, [volumeId], "dep-2"));
      return members;
    });

    it("starts no run for the old Deploy", async () => {
      members = [member("web-1", "writer"), member("web-2", "mirror")];
      await stageMove("web-2", ["web-2"]);
      expect(await run(advancePlacement(organizationId, "dep-1", null, handOver, answered))).toEqual({ kind: "stopped" });
      expect((await harness.pool.query(`select id from volume_run`)).rows).toEqual([]);
      expect(await placement()).toMatchObject({ staged: { primary: web("web-2") }, deploymentId: "dep-2" });
    });

    it("leaves the next Deploy's staged row alone", async () => {
      // Already in place, so the old Deploy would settle the row instead of starting a run.
      members = [member("web-2", "writer"), member("web-1", "mirror")];
      const staged = { primary: web("web-2"), mirrors: [web("web-1")], deletes: [] };
      await run(stagePlacement(organizationId, { environment, volumeId, staged, mirrors: [web("web-1")] }));
      await run(stampPlacements(organizationId, [volumeId], "dep-1"));
      expect(await run(advancePlacement(organizationId, "dep-1", null, handOver, answered))).toEqual({ kind: "done" });
      expect(await placement()).toMatchObject({ staged, deploymentId: "dep-2" });
    });
  });

  const land = (kind: string, args: string, state = "running", phase = "warm") => harness.pool.query(`
    insert into volume_run (organization_id, environment_id, volume_id, volume_name, docker_volume, kind, args, state, phase)
    values ($1, 'env-1', $2, 'data', 'shop-production_vol-vol1', $3, $4, $5, $6) returning id`, [organizationId, volumeId, kind, args, state, phase]);
  const runRows = async () => (await harness.pool.query(`select id, kind, state, message from volume_run order by created_at`)).rows;
  const failedRestores = async () => (await harness.pool.query(`select count(*)::int as count from failed_restore`)).rows[0].count;
  const stageRestore = async () => {
    await run(stagePlacement(organizationId, { environment, volumeId: "vol2", mirrors: [], staged: {
      primary: web("web-1"), mirrors: [], deletes: [],
      seed: { sourceVolumeId: volumeId, sourceName: "data", dockerVolume: "shop-production_vol-vol2", point: { guid: "11", name: "ployz-1", at: 1_790_000_000 } },
    } }));
  };

  it("takes over a warm run it didn't start, and never requests it again", async () => {
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    const other = (await land("move", '{"to":"web-2"}')).rows[0].id;
    await stageMove("web-2", ["web-2"]);
    const started = await run(advancePlacement(organizationId, "dep-1", null, inspect, answered));
    expect(started).toMatchObject({ kind: "wait", wait: { volumeId, step: { mirrors: [] } } });
    if (started.kind !== "wait") throw new Error("expected a run");
    expect(await runRows()).toEqual([
      { id: other, kind: "move", state: "yielded", message: "a Deploy took this run over" },
      { id: started.wait.runId, kind: "move", state: "requested", message: null },
    ]);
    expect(send).toHaveBeenCalledWith(expect.objectContaining({ name: "volume/run.taken", data: { runId: other } }));
    expect(send).toHaveBeenCalledWith(expect.objectContaining({ name: "volume/run.requested", data: expect.objectContaining({ runId: started.wait.runId, took: "move" }) }));

    await endRun(started.wait.runId, "done");
    members = [member("web-2", "writer"), member("web-1", "mirror")];
    expect(await run(advancePlacement(organizationId, "dep-1", started.wait, inspect, answered))).toEqual({ kind: "done" });
    expect((await runRows()).map((row) => row.state)).toEqual(["yielded", "done"]);
  });

  describe("a staged move whose run took over a Move that had cut the primary", () => {
    const restoring = async () => {
      members = [member("web-1", "mirror"), member("web-2", "mirror")];
      const taken = (await land("move", '{"to":"web-2"}')).rows[0].id;
      await stageMove("web-2", ["web-2"]);
      const started = await run(advancePlacement(organizationId, "dep-1", null, inspect, answered));
      if (started.kind !== "wait") throw new Error("expected a run");
      expect((await harness.pool.query(`select kind, args from volume_run where id = $1`, [started.wait.runId])).rows)
        .toEqual([{ kind: "restore", args: { from: "web-2" } }]);
      return { taken, started };
    };

    it("plans again from the copies the fence left, and moves the Volume to web-2", async () => {
      const { taken, started } = await restoring();
      await endRun(started.wait.runId, "failed", "web-1 holds another copy of data");
      members = [member("web-1", "writer"), member("web-2", "mirror")];
      const moving = await run(advancePlacement(organizationId, "dep-1", started.wait, inspect, answered));
      expect(moving).toMatchObject({ kind: "wait", wait: { volumeId, step: { run: { kind: "move", args: { to: "web-2" } }, mirrors: [] } } });
      if (moving.kind !== "wait") throw new Error("expected a run");
      expect(await runRows()).toEqual([
        { id: taken, kind: "move", state: "yielded", message: "a Deploy took this run over" },
        { id: started.wait.runId, kind: "restore", state: "failed", message: "web-1 holds another copy of data" },
        { id: moving.wait.runId, kind: "move", state: "requested", message: null },
      ]);

      await endRun(moving.wait.runId, "done");
      members = [member("web-2", "writer"), member("web-1", "mirror")];
      expect(await run(advancePlacement(organizationId, "dep-1", moving.wait, inspect, answered))).toEqual({ kind: "done" });
      expect(await placement()).toMatchObject({ mirrors: [], staged: null, deploymentId: null });
    });

    it("fails the Deploy when the copies after the fence plan the same restore", async () => {
      const { started } = await restoring();
      await endRun(started.wait.runId, "failed", "web-2 did not answer");
      expect(await run(advancePlacement(organizationId, "dep-1", started.wait, inspect, answered))).toEqual({ kind: "failed", message: "data: web-2 did not answer" });
      expect(await placement()).toMatchObject({ mirrors: [web("web-2")], staged: null, deploymentId: null });
      expect((await runRows()).map((row) => row.state)).toEqual(["yielded", "failed"]);
    });
  });

  it("fails on a run past its return, leaving the change staged for the next Deploy", async () => {
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    const committed = (await land("move", '{"to":"web-2"}', "running", "committed")).rows[0].id;
    await stageMove("web-2", ["web-2"]);
    await run(stagePlacement(organizationId, { environment, volumeId: "vol2", mirrors: [], staged: { primary: web("web-1"), mirrors: [web("web-2")], deletes: [] } }));
    await run(stampPlacements(organizationId, ["vol2"], "dep-1"));
    expect(await run(advancePlacement(organizationId, "dep-1", null, inspect, answered)))
      .toEqual({ kind: "failed", message: "VolumeBusy: data is starting on web-2; it finishes there" });
    expect(await placement()).toMatchObject({ staged: { primary: web("web-2") }, deploymentId: null });
    expect((await run(listPlacements(organizationId))).find((row) => row.volumeId === "vol2")).toMatchObject({ staged: { mirrors: [web("web-2")] }, unreached: true, deploymentId: null });
    expect((await runRows()).map((row) => row.id)).toEqual([committed]);
  });

  it("fails on another Deploy's lane rather than move under its Services, leaving the change staged for the next Deploy", async () => {
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    const deploying = (await land("deploy", '{"deployment":"dep-0","resume":null}')).rows[0].id;
    await stageMove("web-2", ["web-2"]);
    expect(await run(advancePlacement(organizationId, "dep-1", null, inspect, answered)))
      .toEqual({ kind: "failed", message: "data is deploying; the Deploy ends its run" });
    expect(await placement()).toMatchObject({ staged: { primary: web("web-2") }, deploymentId: null });
    expect(await runRows()).toEqual([{ id: deploying, kind: "deploy", state: "running", message: null }]);
    expect(send).not.toHaveBeenCalled();
  });

  it("fails on a run past its return on a restore's source, without failing the restore", async () => {
    const byVolume: MemberInspector = (_, _environment, id) => Effect.succeed(id === "vol2" ? [] : members);
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    await land("move", '{"to":"web-2"}', "running", "committed");
    await stageRestore();
    await run(stampPlacements(organizationId, ["vol2"], "dep-1"));
    expect(await run(advancePlacement(organizationId, "dep-1", null, byVolume, answered)))
      .toEqual({ kind: "failed", message: "VolumeBusy: data is starting on web-2; it finishes there" });
    expect((await run(listPlacements(organizationId))).find((row) => row.volumeId === "vol2")).toMatchObject({ staged: { seed: { sourceVolumeId: volumeId } }, deploymentId: null });
    expect(await failedRestores()).toBe(0);
    expect((await runRows()).map((row) => row.kind)).toEqual(["move"]);
  });

  describe("a lane that lands after the placement read the Volume and before it takes the lane", () => {
    const lanes = [
      { kind: "sync", args: '{"full":false}', state: "running" },
      { kind: "release", args: "{}", state: "requested" },
      { kind: "delete_mirror", args: '{"slot":null,"confirmed_name":null}', state: "requested" },
    ];

    it.each(lanes)("$kind yields to the restore's seed, and the move follows", async ({ kind, args, state }) => {
      const restored = [member("web-1", "writer")];
      const source = [member("web-1", "writer"), member("web-2", "mirror")];
      let landed: string | null = null;
      let seeded = false;
      const landing: MemberInspector = (_, _environment, id) => Effect.promise(async () => {
        if (landed === null) landed = (await land(kind, args, state)).rows[0].id;
        return id === "vol2" ? (seeded ? restored : []) : source;
      });
      await stageMove("web-2", ["web-2"]);
      await stageRestore();
      await run(stampPlacements(organizationId, [volumeId, "vol2"], "dep-1"));

      const seeding = await run(advancePlacement(organizationId, "dep-1", null, landing, answered));
      expect(seeding).toMatchObject({ kind: "wait", wait: { volumeId: "vol2", step: { change: { kind: "seed" } } } });
      if (seeding.kind !== "wait") throw new Error("expected a run");
      expect(await runRows()).toEqual([
        { id: landed, kind, state: "yielded", message: "a Deploy took this run over" },
        { id: seeding.wait.runId, kind: "seed", state: "requested", message: null },
      ]);
      expect(await failedRestores()).toBe(0);
      expect((await run(listPlacements(organizationId))).map((row) => row.deploymentId)).toEqual(["dep-1", "dep-1"]);

      await endRun(seeding.wait.runId, "done");
      seeded = true;
      const moving = await run(advancePlacement(organizationId, "dep-1", seeding.wait, landing, answered));
      expect(moving).toMatchObject({ kind: "wait", wait: { volumeId, step: { change: { kind: "move" } } } });
      if (moving.kind !== "wait") throw new Error("expected a run");
      await endRun(moving.wait.runId, "done");
      members = [member("web-2", "writer"), member("web-1", "mirror")];
      const after: MemberInspector = (_, _environment, id) => Effect.succeed(id === "vol2" ? restored : members);
      expect(await run(advancePlacement(organizationId, "dep-1", moving.wait, after, answered))).toEqual({ kind: "done" });
      expect((await runRows()).map((row) => row.state)).toEqual(["yielded", "done", "done"]);
    });
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
    await run(stampPlacements(organizationId, [volumeId, "vol2"], "dep-1"));

    const seeding = await run(advancePlacement(organizationId, "dep-1", null, byVolume, answered));
    expect(seeding).toMatchObject({ kind: "wait", wait: { volumeId: "vol2", step: { mirrors: [] } } });
    if (seeding.kind !== "wait") throw new Error("expected a run");
    const { rows } = await harness.pool.query(`select volume_id, kind, args from volume_run where id = $1`, [seeding.wait.runId]);
    expect(rows).toEqual([{
      volume_id: volumeId, kind: "seed",
      args: { into: { volumeId: "vol2", dockerVolume: "shop-production_vol-vol2", name: "data-1430" }, guid: "11", name: "ployz-1" },
    }]);

    await endRun(seeding.wait.runId, "done");
    landed = true;
    const moving = await run(advancePlacement(organizationId, "dep-1", seeding.wait, byVolume, answered));
    expect(moving).toMatchObject({ kind: "wait", wait: { volumeId } });
    if (moving.kind !== "wait") throw new Error("expected a run");
    expect((await harness.pool.query(`select kind from volume_run where id = $1`, [moving.wait.runId])).rows).toEqual([{ kind: "move" }]);
    const rowOf = async (id: string) => (await run(listPlacements(organizationId))).find((row) => row.volumeId === id);
    expect(await rowOf("vol2")).toMatchObject({ mirrors: [], staged: null, deploymentId: null });
  });

  it("refuses to stage while no Cluster answers to say which daemons run", async () => {
    const written = await run(unlessPlacementRefused(organizationId, Effect.succeed({ ok: true } as const)));
    expect(written).toMatchObject({ ok: false, refusal: { code: "unanswered" } });
  });

  it("fails with the upgrade message once a Server with storage runs an older daemon", async () => {
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    await stageMove("web-2", ["web-2"]);

    expect(await run(advancePlacement(organizationId, "dep-1", null, inspect, () => Effect.succeed(["db-old"])))).toEqual({
      kind: "failed", message: expect.stringContaining("db-old"),
    });
    expect((await harness.pool.query(`select id from volume_run`)).rows).toEqual([]);
    expect(await placement()).toMatchObject({ staged: { primary: web("web-2") }, unreached: true, deploymentId: null });
  });

  it("fails without starting a run while a Server isn't answering", async () => {
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    await stageMove("web-2", ["web-2"]);

    expect(await run(advancePlacement(organizationId, "dep-1", null, inspect, () => Effect.fail("silent")))).toEqual({
      kind: "failed", message: expect.stringContaining("aren't answering"),
    });
    expect((await harness.pool.query(`select id from volume_run`)).rows).toEqual([]);
    expect(await placement()).toMatchObject({ staged: { primary: web("web-2") }, deploymentId: null });
  });

  it("leaves the source staged while its restore's Store write is in flight", async () => {
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    const seed = { sourceVolumeId: volumeId, sourceName: "data", dockerVolume: "shop-production_vol-vol3", point: { guid: "11", name: "ployz-1", at: 1_790_000_000 } };
    await run(stagePlacement(organizationId, { environment, volumeId, staged: { primary: web("web-2"), mirrors: [], deletes: [] }, mirrors: [web("web-2")] }));
    await harness.pool.query(
      `insert into volume_placement (volume_id, organization_id, environment_id, mirrors, staged) values ('vol3', $1, 'env-1', '[]', $2)`,
      [organizationId, JSON.stringify({ primary: web("web-1"), mirrors: [], deletes: [], seed })],
    );
    await run(stampPlacements(organizationId, [volumeId, "vol3"], "dep-1"));

    expect(await whileStoreWriting(() => run(advancePlacement(organizationId, "dep-1", null, inspect, answered)))).toEqual({ kind: "done" });
    expect((await harness.pool.query(`select id from volume_run`)).rows).toEqual([]);
    const rows = await run(listPlacements(organizationId));
    expect(rows.find((row) => row.volumeId === volumeId)).toMatchObject({ staged: { primary: web("web-2") }, deploymentId: null });
    expect(rows.find((row) => row.volumeId === "vol3")).toMatchObject({ staged: { seed }, deploymentId: null });
  });

  it("keeps a restore staged while its Store write is still in flight", async () => {
    const seed = { sourceVolumeId: volumeId, sourceName: "data", dockerVolume: "shop-production_vol-vol3", point: { guid: "11", name: "ployz-1", at: 1_790_000_000 } };
    await harness.pool.query(
      `insert into volume_placement (volume_id, organization_id, environment_id, mirrors, staged) values ('vol3', $1, 'env-1', '[]', $2)`,
      [organizationId, JSON.stringify({ primary: web("web-1"), mirrors: [], deletes: [], seed })],
    );
    await run(stampPlacements(organizationId, [volumeId], "dep-1"));

    expect(await whileStoreWriting(() => run(advancePlacement(organizationId, "dep-1", null, inspect, answered)))).toEqual({ kind: "done" });
    expect(await placement()).toMatchObject({ volumeId: "vol3", staged: { seed }, deploymentId: null });
  });

  it("refuses to seed a restored Volume whose storage no longer matches its source", async () => {
    members = [member("web-1", "writer")];
    await run(stagePlacement(organizationId, { environment, volumeId: "vol2", mirrors: [], staged: {
      primary: web("web-1"), mirrors: [], deletes: [],
      seed: { sourceVolumeId: volumeId, sourceName: "data", dockerVolume: "shop-production_vol-vol2", point: { guid: "11", name: "ployz-1", at: 1_790_000_000 } },
    } }));
    await run(stampPlacements(organizationId, ["vol2"], "dep-1"));
    restoredStorage = { kind: "docker" };

    expect(await run(advancePlacement(organizationId, "dep-1", null, (_, _environment, id) => Effect.succeed(id === "vol2" ? [] : members), answered))).toEqual({
      kind: "failed",
      message: "data-1430: data-1430's storage no longer matches data's, so the restore can't land on it. Discard the restore and restore again.",
      restore: { volumeId: "vol2", environment },
    });
    expect((await harness.pool.query(`select id from volume_run`)).rows).toEqual([]);
  });

  describe("refreshing mirrors", () => {
    const copy = (server: string, role: "writer" | "slot") => ({ machine_id: web(server), name: "shop-production_vol-vol1", role });
    let asked: string[];
    beforeEach(() => { asked = []; });
    const ask: CopyAsk = (_asker, machine, _expect, want: WantedCopy) => {
      asked.push(`${want.kind} ${machine.name}`);
      const newest = { name: "w-1-5", guid: "12", created_unix_seconds: 1_790_000_100 };
      return Effect.succeed<CopyAnswer>(want.kind === "snapshot" ? { ok: true, copy: { ...COPIES.writer, newest } } : { ok: true, copy: COPIES.pulling });
    };
    const refresh = (copies: CopyObservation["copies"], blockers: string[] = []) =>
      run(refreshMirrors(organizationId, null, () => Effect.succeed(copies), () => Effect.succeed(blockers), () => Effect.succeed(members), ask));
    const runKinds = async () => (await harness.pool.query(`select kind from volume_run`)).rows.map((row) => row.kind);

    it("syncs a copy the dashboard has no record for, as the CLI made it a mirror, and inserts no run", async () => {
      members = [member("web-1", "writer"), member("web-2", "mirror")];
      expect(await refresh([copy("web-1", "writer"), copy("web-2", "slot")])).toMatchObject([{ lines: ["data-web-2 is pulling w-1-5"] }]);
      expect(asked).toEqual(["snapshot web-1", "mirror web-2"]);
      expect(await runKinds()).toEqual([]);
    });

    it("never refreshes a detached copy, and says the Volume has no mirror", async () => {
      await run(stagePlacement(organizationId, { environment, volumeId, staged: null, mirrors: [] }));
      members = [member("web-2", "writer"), member("web-1", "mirror")];
      expect(await refresh([copy("web-2", "writer"), copy("web-1", "slot")])).toMatchObject([{ lines: ["data has no mirror"] }]);
      expect(asked).toEqual([]);
    });

    it("requests the demoting run when an old primary came back", async () => {
      await run(stagePlacement(organizationId, { environment, volumeId, staged: null, mirrors: [] }));
      members = [member("web-2", "writer"), member("web-1", "writer")];
      expect(await refresh([copy("web-2", "writer"), copy("web-1", "writer")])).toMatchObject([{ sync: { kind: "demoting" } }]);
      expect(await runKinds()).toEqual(["sync"]);
    });

    it("leaves a Volume a Deploy is moving to that Deploy, and says so", async () => {
      await stageMove("web-2", ["web-2"]);
      members = [member("web-1", "writer"), member("web-2", "mirror")];
      expect(await refresh([copy("web-1", "writer"), copy("web-2", "slot")])).toMatchObject([{ sync: { kind: "deploying" } }]);
      expect(asked).toEqual([]);
    });

    it("skips an organization while a Server with storage runs an older daemon", async () => {
      expect(await refresh([copy("web-1", "writer"), copy("web-2", "slot")], ["db-old"])).toEqual([]);
      expect(await runKinds()).toEqual([]);
    });
  });

  describe("gating a staged placement on every Server with storage", () => {
    const stage = (read: () => Effect.Effect<readonly string[], unknown>) => run(unlessPlacementRefused(organizationId,
      stagePlacement(organizationId, { environment, volumeId, staged: { primary: web("web-2"), mirrors: [], deletes: [] }, mirrors: [] }), read));

    it("refuses while one runs an older daemon, and stages nothing", async () => {
      expect(await stage(() => Effect.succeed(["db-old"]))).toEqual({ ok: false, refusal: {
        code: "upgrade_required", message: "Every Server with storage needs a newer Ployz. Upgrade db-old first.", details: { servers: ["db-old"] },
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
