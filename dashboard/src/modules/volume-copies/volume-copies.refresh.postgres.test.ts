import type { ConfigStore, MachineId, WantedCopy } from "@ployz/sdk";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { mirrorStatuses, refreshMirror } from "#/modules/volume-copies/volume-copies.refresh.server";
import type { CopyAsk } from "#/modules/volume-placement/mirror-sync.server";
import { stagePlacement, stampPlacements } from "#/modules/volume-placement/volume-placement.server";
import type { Member } from "#/modules/volume-run/volume-run";
import { type CopyAnswer, requestVolumeRun } from "#/modules/volume-run/volume-run.server";
import { COPIES, member } from "#/modules/volume-run/volume-run.test-fixture";
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
  // Runs at each Volume read, so a test can act between Refresh's mirror check and its request.
  let between: (() => Promise<void>) | null;
  let members: Member[];
  let asked: WantedCopy["kind"][];

  const run = <A, E>(operation: Effect.Effect<A, E, Database | SecretEncryption | CloudStore | InngestClient | OrganizationRuntime>) => harness.runEffect(operation.pipe(
    Effect.provideService(InngestClient, inngest),
    Effect.provideService(OrganizationRuntime, { cancel: () => Effect.void, open: () => Effect.succeed({ status: "no_connection" as const }) }),
    Effect.provideService(CloudStore, {
      open: Effect.succeed(asTestDouble<ConfigStore>()({
        read: async (_: string, query: { query: string }) => {
          if (query.query === "namespaces") return { namespaces: [{ namespace: "shop-production", project: "shop", environment: "production" }] };
          if (query.query === "deployment") return { status: "queued", in_flight: true, environment: { id: "env-1", project: "shop", name: "production" } };
          await between?.();
          return {
            environment: { id: "env-1", project: "shop", name: "production", revision: 1 },
            volumes: [{ id: volumeId, name: "data", storage: { kind: "provisioned", maximumBytes: 1 } }],
          };
        },
      })),
    }),
  ));
  const ask: CopyAsk = (_asker, _machine, _expect, want: WantedCopy) => {
    asked.push(want.kind);
    const newest = { name: "w-1-5", guid: "12", created_unix_seconds: 1_790_000_100 };
    return Effect.succeed<CopyAnswer>(want.kind === "snapshot" ? { ok: true, copy: { ...COPIES.writer, newest } } : { ok: true, copy: COPIES.pulling });
  };
  const refresh = () => run(refreshMirror(caller, environment, volumeId, () => Effect.succeed(blockers), () => Effect.succeed(members), () => Effect.succeed(members), ask));
  const runs = async () => (await harness.pool.query(`select kind, state from volume_run order by created_at`)).rows;

  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });
  beforeEach(async () => {
    blockers = [];
    between = null;
    asked = [];
    members = [member("web-1", "writer"), member("web-2", "mirror")];
    await harness.pool.query(`
      truncate table organization cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
      insert into volume_placement (volume_id, organization_id, environment_id, mirrors) values ('${volumeId}', '${organizationId}', 'env-1', '["${web("web-2")}"]');
    `);
  });

  it("snapshots the writer and hands the pull to the mirror, with no run", async () => {
    expect(await refresh()).toEqual({ ok: true, lines: ["data-web-2 is pulling w-1-5"] });
    expect(asked).toEqual(["snapshot", "mirror"]);
    expect(await runs()).toEqual([]);
  });

  it("asks the mirror for its pull again and says it is still pulling, instead of snapshotting again", async () => {
    members = [member("web-1", "writer"), member("web-2", "pulling")];
    expect(await refresh()).toEqual({ ok: true, lines: ["still pulling w-1-4 on data-web-2"] });
    expect(asked).toEqual(["mirror"]);
  });

  it("names the run that covers the Volume and takes no snapshot", async () => {
    const move = await run(requestVolumeRun(caller, { volumeId, environment, kind: "move", args: { to: "web-2" } }));
    if (!move.ok) throw new Error("expected a run");
    expect(await refresh()).toEqual({ ok: false, refusal: { code: "conflict", message: `data refresh is covered by move run ${move.run.id}`, details: null } });
    expect(asked).toEqual([]);
    expect(await runs()).toEqual([{ kind: "move", state: "requested" }]);
  });

  it("leaves a Volume a Deploy is moving to that Deploy", async () => {
    await run(stagePlacement(organizationId, { environment, volumeId, staged: { primary: web("web-2"), mirrors: [], deletes: [] }, mirrors: [web("web-2")] }));
    await run(stampPlacements(organizationId, [volumeId], "dep-1"));
    expect(await refresh()).toMatchObject({ ok: false, refusal: { code: "conflict", message: "skipped data: A Deploy is moving this Volume. Wait for it to finish." } });
    expect(asked).toEqual([]);
  });

  it("refuses when a Deploy stamps the Volume while the sync reads it", async () => {
    await run(stagePlacement(organizationId, { environment, volumeId, staged: { primary: web("web-2"), mirrors: [], deletes: [] }, mirrors: [web("web-2")] }));
    between = async () => { await run(stampPlacements(organizationId, [volumeId], "dep-1")); };
    expect(await refresh()).toMatchObject({ ok: false, refusal: { message: "skipped data: A Deploy is moving this Volume. Wait for it to finish." } });
    expect(asked).toEqual([]);
  });

  it("refreshes a mirror the CLI made, with no placement row, as the 15-minute sync does", async () => {
    await harness.pool.query(`delete from volume_placement`);
    expect(await refresh()).toEqual({ ok: true, lines: ["data-web-2 is pulling w-1-5"] });
    expect(await runs()).toEqual([]);
  });

  it("refuses when the Servers can't say where the Volume's copies are", async () => {
    expect(await run(refreshMirror(caller, environment, volumeId, () => Effect.succeed(blockers), () => Effect.fail(new Error("down")), () => Effect.fail(new Error("down")), ask)))
      .toMatchObject({ ok: false, refusal: { code: "unanswered" } });
  });

  it("names the mirror's Server when it is silent", async () => {
    members = [member("web-1", "writer"), member("web-2", "unanswered")];
    expect(await refresh()).toEqual({ ok: false, refusal: { code: "unanswered", message: "web-2 isn't answering", details: null } });
  });

  it("names the silent Server when the runtime can't say which Servers answer", async () => {
    members = [member("web-1", "writer"), member("web-2", "unanswered")];
    expect(await run(refreshMirror(caller, environment, volumeId, () => Effect.fail("down"), () => Effect.succeed(members), () => Effect.succeed(members), ask)))
      .toEqual({ ok: false, refusal: { code: "unanswered", message: "web-2 isn't answering", details: null } });
  });

  it("refuses a Volume with no mirror", async () => {
    await harness.pool.query(`update volume_placement set mirrors = '[]'`);
    expect(await refresh()).toEqual({ ok: false, refusal: { code: "conflict", message: "data has no mirror", details: null } });
    await harness.pool.query(`delete from volume_placement`);
    members = [member("web-1", "writer")];
    expect(await refresh()).toEqual({ ok: false, refusal: { code: "conflict", message: "data has no mirror", details: null } });
    expect(await runs()).toEqual([]);
  });

  it("refuses while a Server with storage runs an older daemon", async () => {
    blockers = ["db-old"];
    expect(await refresh()).toMatchObject({ ok: false, refusal: { code: "upgrade_required", details: { servers: ["db-old"] } } });
  });

  it("reads what the syncs saw of each mirror", async () => {
    expect(await run(mirrorStatuses(organizationId, volumeId))).toEqual([]);
    members = [member("web-1", "writer"), member("web-2", "failed")];
    expect(await refresh()).toMatchObject({ ok: true, lines: ["data-web-2 could not receive w-1-4: no space", "data-web-2 is pulling w-1-5"] });
    expect(await run(mirrorStatuses(organizationId, volumeId))).toMatchObject([{ machine: web("web-2"), failure: "no space", okAt: null }]);
  });
});
