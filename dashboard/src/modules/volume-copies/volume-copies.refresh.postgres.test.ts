import { type Client, type ConfigStore, type MachineId, RpcError, type VolumeCopy, type VolumeSwitchRequest, type WantedCopy } from "@ployz/sdk";
import { Effect, Fiber, Layer } from "effect";
import { TestClock } from "effect/testing";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import { loadOrganizationConnections } from "#/modules/machines/connections.server";
import { clusterRead, makeOrganizationRuntimeLayer, OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { mirrorStatuses, refreshMirror, SYNC_CLUSTER_BUDGET_MS } from "#/modules/volume-copies/volume-copies.refresh.server";
import type { CopyAsk } from "#/modules/volume-placement/mirror-sync.server";
import { stagePlacement, stampPlacements } from "#/modules/volume-placement/volume-placement.server";
import type { Member } from "#/modules/volume-run/volume-run";
import { type CopyAnswer, requestVolumeRun } from "#/modules/volume-run/volume-run.server";
import { COPIES, member } from "#/modules/volume-run/volume-run.test-fixture";
import { Database } from "#/server/database.server";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";
import { noPairingChanges } from "#/test/organization-runtime";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const organizationId = "00000000-0000-4000-8000-000000000f04";
const caller = { userId: null, organizationId };
const volumeId = "vol1";
const environment = { project: "shop", environment: "production" };
const web = (name: string) => name.padEnd(32, "0") as MachineId;
const inngest = new Inngest({ id: "volume-copies-refresh-test" });
vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

const SNAPSHOT = { copy: { ...COPIES.writer, newest: { name: "w-1-5", guid: "12", created_unix_seconds: 1_790_000_100 } } };
// The SDK's own 10s timeout on a target Machine's RPC.
const TIMED_OUT = new RpcError({ code: "unavailable", message: "target Machine RPC timed out", details: null });

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
  // The real askCopy, over a session to a paired Cluster whose SetVolumeCopy answers `answer`.
  const pairedCluster = async (answer: (want: WantedCopy) => Promise<{ readonly copy: VolumeCopy }>) => {
    const encryption = makeSecretEncryption("test-encryption-secret");
    await harness.pool.query(`insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_claim_machine_id, founder_machine_id)
      values ($1, $2, $3, $3)`, [organizationId, encryption.encrypt("pairing"), "1".repeat(32)]);
    await harness.pool.query(`insert into organization_machine (organization_id, machine_id, cluster_key, encrypted_capability)
      values ($1, $2, $3, $4)`, [organizationId, "1".repeat(32), hashEnrollmentToken("pairing"), encryption.encrypt("ployz1:test")]);
    return makeOrganizationRuntimeLayer(
      (id) => loadOrganizationConnections(id).pipe(Effect.provideService(Database, harness.database), Effect.provideService(SecretEncryption, encryption)),
      noPairingChanges,
    ).pipe(Layer.provide(makePloyzLayer({
      connect: async () => asTestDouble<Client>()({
        close: async () => {},
        volumeSwitch: async (_machine: MachineId, request: VolumeSwitchRequest) => {
          if (request.command !== "set_volume_copy") throw new Error(`unexpected ${request.command}`);
          return answer(request.payload.want);
        },
      }),
    })));
  };
  const refreshOn = (cluster: Layer.Layer<OrganizationRuntime>) =>
    refreshMirror(caller, environment, volumeId, () => Effect.succeed(blockers), () => Effect.succeed(members), () => Effect.succeed(members)).pipe(Effect.provide(cluster));

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

  it("refuses when the mirror refuses the pull, so no success is reported for a pull that never started", async () => {
    const mismatch: CopyAsk = (asker, machine, expect, want) => want.kind === "mirror"
      ? Effect.succeed<CopyAnswer>({ ok: false, reason: "mismatch", copy: COPIES.mirror, message: "data-web-2 changed" })
      : ask(asker, machine, expect, want);
    expect(await run(refreshMirror(caller, environment, volumeId, () => Effect.succeed(blockers), () => Effect.succeed(members), () => Effect.succeed(members), mismatch)))
      .toEqual({ ok: false, refusal: { code: "conflict", message: "data-web-2 refused the pull: data-web-2 changed", details: null } });
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
    members = [member("web-1", "writer"), member("web-2", "old")];
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

  it("answers once its Cluster time runs out on a pull the mirror never answers, and lets the sync's SQL finish", async () => {
    members = [member("web-1", "writer"), member("web-2", "failed")];
    await harness.pool.query(`insert into volume_mirror_status (organization_id, volume_id, machine_id, failed_at, failed_snapshot, failure, updated_at)
      values ($1, $2, $3, now(), 'w-1-3', 'old', now())`, [organizationId, volumeId, web("web-2")]);
    const holder = await harness.pool.connect();
    await holder.query(`begin; select * from volume_mirror_status for update`);
    const silent: CopyAsk = (asker, machine, expect, want) => want.kind === "mirror"
      ? Effect.sync(() => asked.push(want.kind)).pipe(Effect.andThen(clusterRead(Effect.never)))
      : ask(asker, machine, expect, want);
    const waitFor = (check: () => Promise<boolean> | boolean) => Effect.promise(() => vi.waitFor(async () => {
      if (!(await check())) throw new Error("not yet");
    }, { timeout: 10_000 }));
    const held = Effect.promise(() => new Promise<"held">((resolve) => { setTimeout(() => resolve("held"), 10_000); }));

    const answer = await run(Effect.gen(function* () {
      const fiber = yield* Effect.forkDetach(refreshMirror(caller, environment, volumeId, () => Effect.succeed(blockers), () => Effect.succeed(members), () => Effect.succeed(members), silent));
      yield* waitFor(async () => (await harness.pool.query(`select 1 from pg_stat_activity where wait_event_type = 'Lock'`)).rowCount === 1);
      yield* TestClock.adjust(2 * SYNC_CLUSTER_BUDGET_MS);
      yield* Effect.promise(() => holder.query(`commit`));
      yield* waitFor(() => asked.includes("mirror"));
      yield* TestClock.adjust(SYNC_CLUSTER_BUDGET_MS);
      return yield* Effect.raceFirst(Fiber.join(fiber), held);
    }).pipe(Effect.provide(TestClock.layer()))).finally(() => holder.release());

    expect(answer).toEqual({
      ok: false,
      refusal: {
        code: "unanswered",
        message: "data-web-2 could not receive w-1-4: no space; web-2 didn't answer in time, so data's sync may have reached it; volume sync data again says where its mirrors got to",
        details: null,
      },
    });
    expect(await run(mirrorStatuses(organizationId, volumeId))).toMatchObject([{ machine: web("web-2"), failure: "no space" }]);
  }, 30_000);

  it("opens each session's SQL outside the Cluster time, and answers once a SetVolumeCopy the mirror never answers spends it", async () => {
    const switched: string[] = [];
    const cluster = await pairedCluster((want) => {
      switched.push(want.kind);
      return want.kind === "mirror" ? new Promise<never>(() => {}) : Promise.resolve(SNAPSHOT);
    });
    const holder = await harness.pool.connect();
    await holder.query(`begin; lock table organization_pairing in access exclusive mode`);
    const waitFor = (check: () => Promise<boolean> | boolean) => Effect.promise(() => vi.waitFor(async () => {
      if (!(await check())) throw new Error("not yet");
    }, { timeout: 10_000 }));
    const held = Effect.promise(() => new Promise<"held">((resolve) => { setTimeout(() => resolve("held"), 10_000); }));

    const answer = await run(Effect.gen(function* () {
      const fiber = yield* Effect.forkDetach(refreshOn(cluster));
      yield* waitFor(async () => (await harness.pool.query(`select 1 from pg_stat_activity where wait_event_type = 'Lock'`)).rowCount === 1);
      yield* TestClock.adjust(2 * SYNC_CLUSTER_BUDGET_MS);
      yield* Effect.promise(() => holder.query(`commit`));
      const early = yield* Effect.raceFirst(waitFor(() => switched.includes("mirror")).pipe(Effect.as(null)), Fiber.join(fiber));
      if (early !== null) return early;
      yield* TestClock.adjust(SYNC_CLUSTER_BUDGET_MS);
      return yield* Effect.raceFirst(Fiber.join(fiber), held);
    }).pipe(Effect.provide(TestClock.layer()))).finally(() => holder.release());

    expect(answer).toEqual({
      ok: false,
      refusal: { code: "unanswered", message: "web-2 didn't answer in time, so data's sync may have reached it; volume sync data again says where its mirrors got to", details: null },
    });
    expect(switched).toEqual(["snapshot", "mirror"]);
  }, 30_000);

  it("answers with the round's pull failures once its Cluster time runs out, and never says a pull it didn't send may have reached a mirror", async () => {
    members = [member("web-1", "writer"), member("web-2", "failed"), member("web-3", "mirror")];
    const switched: string[] = [];
    const cluster = await pairedCluster((want) => {
      switched.push(want.kind);
      return want.kind === "mirror" ? new Promise<never>(() => {}) : Promise.resolve(SNAPSHOT);
    });
    const held = Effect.promise(() => new Promise<"held">((resolve) => { setTimeout(() => resolve("held"), 10_000); }));

    const answer = await run(Effect.gen(function* () {
      const fiber = yield* Effect.forkDetach(refreshOn(cluster));
      yield* Effect.promise(() => vi.waitFor(() => expect(switched).toContain("mirror"), { timeout: 10_000 }));
      yield* TestClock.adjust(SYNC_CLUSTER_BUDGET_MS);
      return yield* Effect.raceFirst(Fiber.join(fiber), held);
    }).pipe(Effect.provide(TestClock.layer())));

    expect(answer).toEqual({
      ok: false,
      refusal: {
        code: "unanswered",
        message: [
          "data-web-2 could not receive w-1-4: no space",
          "web-2 didn't answer in time, so data's sync may have reached it; volume sync data again says where its mirrors got to",
          "didn't ask web-3 to pull data, the sync ran out of time",
        ].join("; "),
        details: null,
      },
    });
    expect(switched).toEqual(["snapshot", "mirror"]);
  }, 30_000);

  it("says a hand-off the mirror's Server timed out on may have reached it, and blames no mirror for it", async () => {
    const cluster = await pairedCluster((want) => want.kind === "mirror" ? Promise.reject(TIMED_OUT) : Promise.resolve(SNAPSHOT));
    expect(await run(refreshOn(cluster))).toEqual({
      ok: false,
      refusal: { code: "unanswered", message: "web-2 didn't answer in time, so data's sync may have reached it; volume sync data again says where its mirrors got to", details: null },
    });
    expect((await harness.pool.query(`select failure from volume_mirror_status where failure is not null`)).rows).toEqual([]);
  });

  it("answers with the round's pull failures when the writer's snapshot times out", async () => {
    members = [member("web-1", "writer"), member("web-2", "failed")];
    const cluster = await pairedCluster(() => Promise.reject(TIMED_OUT));
    expect(await run(refreshOn(cluster))).toEqual({
      ok: false,
      refusal: {
        code: "unanswered",
        message: "data-web-2 could not receive w-1-4: no space; web-1 didn't answer in time, so data's sync may have reached it; volume sync data again says where its mirrors got to",
        details: null,
      },
    });
  });

  it("reads what the syncs saw of each mirror", async () => {
    expect(await run(mirrorStatuses(organizationId, volumeId))).toEqual([]);
    members = [member("web-1", "writer"), member("web-2", "failed")];
    expect(await refresh()).toMatchObject({ ok: true, lines: ["data-web-2 could not receive w-1-4: no space", "data-web-2 is pulling w-1-5"] });
    expect(await run(mirrorStatuses(organizationId, volumeId))).toMatchObject([{ machine: web("web-2"), failure: "no space", okAt: null }]);
  });
});
