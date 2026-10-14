import type { ConfigStore, MachineId, WantedCopy } from "@ployz/sdk";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { type CopyAsk, syncMirrors } from "#/modules/volume-placement/mirror-sync.server";
import type { Member } from "#/modules/volume-run/volume-run";
import { type CopyAnswer, requestVolumeRun } from "#/modules/volume-run/volume-run.server";
import { COPIES, member } from "#/modules/volume-run/volume-run.test-fixture";
import type { Database } from "#/server/database.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";

const organizationId = "00000000-0000-4000-8000-000000000f06";
const volumeId = "vol1";
const environment = { project: "shop", environment: "production" };
const volume = { volumeId, name: "db", dockerVolume: "shop-production_vol-vol1", environment };
const web = (name: string) => name.padEnd(32, "0") as MachineId;
const inngest = new Inngest({ id: "mirror-sync-test" });
vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
const NEWEST = { name: "w-1-5", guid: "12", created_unix_seconds: 1_790_000_100 };

describe("the mirror sync", () => {
  let harness: PostgresTestHarness;
  let members: Member[];
  let asked: string[];
  let mirrorAnswer: CopyAnswer;
  let gens: number[];
  let onDeployment: () => Promise<void>;

  const run = <A, E>(operation: Effect.Effect<A, E, Database | SecretEncryption | CloudStore | InngestClient | OrganizationRuntime>) => harness.runEffect(operation.pipe(
    Effect.provideService(InngestClient, inngest),
    Effect.provideService(OrganizationRuntime, { cancel: () => Effect.void, open: () => Effect.succeed({ status: "no_connection" as const }) }),
    Effect.provideService(CloudStore, {
      open: Effect.succeed(asTestDouble<ConfigStore>()({
        read: async (_: string, query: { query: string }) => {
          if (query.query === "deployment") {
            await onDeployment();
            return { in_flight: false };
          }
          if (query.query === "namespaces") return { namespaces: [{ namespace: "shop-production", project: "shop", environment: "production" }] };
          return {
            environment: { id: "env-1", project: "shop", name: "production", revision: 1 },
            volumes: [{ id: volumeId, name: "db", storage: { kind: "provisioned", maximumBytes: 1 } }],
          };
        },
      })),
    }),
  ));
  const ask: CopyAsk = (asker, machine, _expect, want: WantedCopy) => {
    asked.push(`${want.kind} ${machine.name}`);
    gens.push(asker.gen);
    return Effect.succeed<CopyAnswer>(want.kind === "snapshot" ? { ok: true, copy: { ...COPIES.writer, newest: NEWEST } } : mirrorAnswer);
  };
  const sync = () => run(syncMirrors(organizationId, volume, () => Effect.succeed(members), ask));
  const runs = async () => (await harness.pool.query(`select kind from volume_run`)).rows.map((row) => row.kind);
  const status = async () => (await harness.pool.query(`select ok_snapshot, ok_at, failed_snapshot, failed_at, failure from volume_mirror_status`)).rows;

  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });
  beforeEach(async () => {
    asked = [];
    gens = [];
    onDeployment = async () => {};
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    mirrorAnswer = { ok: true, copy: COPIES.pulling };
    await harness.pool.query(`
      truncate table organization cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
      insert into volume_placement (volume_id, organization_id, environment_id, mirrors) values ('${volumeId}', '${organizationId}', 'env-1', '["${web("web-2")}"]');
    `);
  });

  it("takes one snapshot and makes one hand-off, with no run", async () => {
    expect((await sync()).lines).toEqual(["db-web-2 is pulling w-1-5"]);
    expect(asked).toEqual(["snapshot web-1", "mirror web-2"]);
    expect(await runs()).toEqual([]);
  });

  it("takes no snapshot while a lane is open, and names the lane", async () => {
    const release = await run(requestVolumeRun({ userId: null, organizationId }, { volumeId, environment, kind: "release", args: {} }));
    if (!release.ok) throw new Error(release.refusal.message);
    expect((await sync()).lines).toEqual([`skipped db, release run ${release.run.id} is open`]);
    expect(asked).toEqual([]);
  });

  it("reports a failed pull once, before the lane check, even with a move open", async () => {
    const move = await run(requestVolumeRun({ userId: null, organizationId }, { volumeId, environment, kind: "move", args: { to: "web-2" } }));
    if (!move.ok) throw new Error(move.refusal.message);
    members = [member("web-1", "writer"), member("web-2", "failed")];
    expect((await sync()).lines).toEqual(["db-web-2 could not receive w-1-4: no space", `db refresh is covered by move run ${move.run.id}`]);
    expect((await sync()).lines).toEqual([`db refresh is covered by move run ${move.run.id}`]);
    expect(asked).toEqual([]);
  });

  it("logs a Busy reply as still pulling and does not fail", async () => {
    mirrorAnswer = { ok: false, reason: "busy", copy: COPIES.pulling, message: "web-2 is busy with db" };
    expect(await sync()).toMatchObject({ sync: { kind: "synced" }, lines: ["still pulling w-1-4 on db-web-2"] });
  });

  it("takes no snapshot while a pull runs, so the writer keeps its source", async () => {
    members = [member("web-1", "writer"), member("web-2", "pulling")];
    expect((await sync()).lines).toEqual(["still pulling w-1-4 on db-web-2"]);
    expect(asked).toEqual(["mirror web-2"]);
  });

  it("finishes a pull that landed, then refreshes the mirror again", async () => {
    members = [member("web-1", "writer"), { ...member("web-2", "mirror"), view: { copy: { ...COPIES.mirror, pull: { target: "w-1-4", failure: null } } } } as Member];
    const finishing: CopyAsk = (asker, machine, expected, want) => want.kind === "mirror" && want.target === "w-1-4"
      ? Effect.sync(() => { asked.push(`finish ${machine.name}`); return { ok: true, copy: COPIES.mirror } satisfies CopyAnswer; })
      : ask(asker, machine, expected, want);
    expect((await run(syncMirrors(organizationId, volume, () => Effect.succeed(members), finishing))).lines).toEqual(["db-web-2 is pulling w-1-5"]);
    expect(asked).toEqual(["finish web-2", "snapshot web-1", "mirror web-2"]);
    expect(await status()).toEqual([expect.objectContaining({ ok_snapshot: "w-1-4" })]);
  });

  it("carries a gen below any lane opened after its lane check", async () => {
    await harness.pool.query(`update volume_placement set deployment_id = 'dep-1'`);
    let moveGen = 0;
    onDeployment = async () => {
      const move = await run(requestVolumeRun({ userId: null, organizationId }, { volumeId, environment, kind: "move", args: { to: "web-2" } }));
      if (!move.ok) throw new Error(move.refusal.message);
      moveGen = (await harness.pool.query(`select gen from volume_run where id = $1`, [move.run.id])).rows[0].gen;
    };
    await sync();
    expect(gens.length).toBeGreaterThan(0);
    expect(gens.every((gen) => gen < Number(moveGen))).toBe(true);
  });

  it("keeps an older failure when a newer pull lands", async () => {
    members = [member("web-1", "writer"), member("web-2", "failed")];
    await sync();
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    await sync();
    expect(await status()).toEqual([expect.objectContaining({ ok_snapshot: "w-1-4", failed_snapshot: "w-1-4", failure: "no space" })]);
    const [row] = await status();
    expect(row.ok_at > row.failed_at).toBe(true);
  });

  it("dates a mirror that never catches a written Volume by the snapshot it last landed", async () => {
    const landed = { name: "w-1-4", guid: "11", created_unix_seconds: 1_790_000_050 };
    members = [member("web-1", "writer"), member("web-2", "behind")];
    await sync();
    members = [member("web-1", "writer"), { ...member("web-2", "mirror"), view: { copy: { ...COPIES.mirror, newest: landed } } } as Member];
    await sync();
    const matched = async () => (await harness.pool.query(`select matched_at from volume_mirror_status`)).rows[0].matched_at;
    expect(await matched()).toEqual(new Date(landed.created_unix_seconds * 1000));
    await harness.pool.query(`update volume_mirror_status set ok_snapshot = 'w-1-3'`);
    members = [member("web-1", "writer"), { ...member("web-2", "mirror"), view: { copy: { ...COPIES.mirror, newest: { ...landed, created_unix_seconds: 1_790_000_010 } } } } as Member];
    await sync();
    expect(await matched()).toEqual(new Date(landed.created_unix_seconds * 1000));
  });

  it("says the mirror already holds the writer's newest", async () => {
    const [writer, mirror] = [member("web-1", "writer"), member("web-2", "mirror")];
    if (!mirror.answered) throw new Error("fixture");
    members = [writer, { ...mirror, view: { copy: { ...COPIES.mirror, newest: NEWEST } } }];
    expect((await sync()).lines).toEqual(["db-web-2 already holds w-1-5"]);
    expect(asked).toEqual(["snapshot web-1"]);
  });
});
