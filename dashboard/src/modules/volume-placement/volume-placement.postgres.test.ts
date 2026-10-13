import { createRequire } from "node:module";
import type * as PloyzSdk from "@ployz/sdk";
import type { ConfigCommand, ConfigCommitted, ConfigStore, MachineId } from "@ployz/sdk";
import { ConfigProvider, Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { GithubApi } from "#/modules/github/github-observation.api";
import { InngestClient } from "#/modules/inngest/client";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import {
  deleteOldCopy, discardPlacement, listPlacements, requestUnclaimedRun, restoreSettled, stagePlacement, stampPlacements, writeAdmitted,
} from "#/modules/volume-placement/volume-placement.server";
import type { Member } from "#/modules/volume-run/volume-run";
import { member } from "#/modules/volume-run/volume-run.test-fixture";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";
import { testConfigEnvironment } from "#/test/config-environment";
import { fakeGithubApi } from "#/test/fake-github";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const { RpcError } = createRequire(import.meta.url)("@ployz/sdk") as Pick<typeof PloyzSdk, "RpcError">;

type StoreWrites = AppConfig | GithubApi | InngestClient | OrganizationRuntime | SecretEncryption;

const organizationId = "00000000-0000-4000-8000-000000000f01";
const volumeId = "vol1";
const environment = { project: "shop", environment: "production" };
const web = (name: string) => name.padEnd(32, "0") as MachineId;

/** Polls until `ready`, or until `settled` ends first. */
const waitFor = async (ready: () => Promise<boolean>, settled: Promise<unknown>) => {
  let done = false;
  void settled.finally(() => { done = true; });
  while (!done && !(await ready())) await new Promise((resolve) => setTimeout(resolve, 10));
};

describe("volume placement", () => {
  let harness: PostgresTestHarness;
  let inFlight: boolean;
  let unreadable: boolean;
  let deleting: boolean;
  // Runs once, as the Store answers `query`: a Deploy admitted while the edit is between its check and its write.
  let between: { readonly query: string; readonly run: () => Promise<void> } | null;

  // A placement with no restore staged never writes the Store; its writer's services only fill the type.
  const run = <A, E>(operation: Effect.Effect<A, E, Database | CloudStore | StoreWrites>) => harness.runEffect(operation.pipe(
    Effect.provide(AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({ env: { ...testConfigEnvironment(), DATABASE_URL: "postgres://unused@localhost/unused" } }))))),
    Effect.provideService(GithubApi, fakeGithubApi().service),
    Effect.provideService(InngestClient, new Inngest({ id: "volume-placement-test" })),
    Effect.provideService(OrganizationRuntime, { cancel: () => Effect.void, open: () => Effect.die("no Cluster") }),
    Effect.provideService(CloudStore, {
      open: Effect.succeed(asTestDouble<ConfigStore>()({
        read: async (_: string, query: { query: string; environment?: { environment: string | null } }) => {
          if (between?.query === query.query) {
            const { run: admit } = between;
            between = null;
            await admit();
          }
          if (query.query === "deployment" && unreadable) throw new RpcError({ code: "unavailable", message: "Store down", details: null });
          if (query.query === "namespaces") {
            return { namespaces: ["production", "staging"].map((name) => ({ namespace: `shop-${name}`, project: "shop", environment: name })) };
          }
          if (query.query === "deployment") return { in_flight: inFlight, nodes: [] };
          if (query.query === "diff") return { version: "diff-7" };
          if (query.environment?.environment === "staging") {
            return {
              environment: { id: "env-2", project: "shop", name: "staging", revision: 1 },
              volumes: [{ id: "vol2", name: "cache", storage: { kind: "provisioned", maximumBytes: 1 }, change: null }],
            };
          }
          return {
            environment: { id: "env-1", project: "shop", name: "production", revision: 1 },
            volumes: [{ id: volumeId, name: "data", storage: { kind: "provisioned", maximumBytes: 1 }, change: deleting ? "delete" : null }],
          };
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
    unreadable = false;
    deleting = false;
    between = null;
    await harness.pool.query(`
      truncate table organization cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
    `);
  });

  it("records the mirrors seen at the first stage, and keeps them when the user stages again", async () => {
    await stage("web-2");
    await stage("web-3");
    expect(await run(listPlacements(organizationId))).toEqual([{
      volumeId, environmentId: "env-1", mirrors: [web("web-2")], staged: { primary: web("web-3"), mirrors: [], deletes: [] }, unreached: false, deploymentId: null,
    }]);
  });

  it("hands the staged placement to the Deploy, and refuses Discard and new edits while it runs", async () => {
    await stage("web-2");
    await run(stampPlacements(organizationId, [volumeId], "dep-1"));
    expect((await run(listPlacements(organizationId)))[0]?.deploymentId).toBe("dep-1");
    expect(await run(discardPlacement(organizationId, null, volumeId, environment))).toMatchObject({ ok: false, refusal: { code: "conflict" } });
    expect(await stage("web-3")).toMatchObject({ ok: false });
    expect((await run(listPlacements(organizationId)))[0]?.staged?.primary).toBe(web("web-2"));
  });

  it("discards once the Deploy has ended", async () => {
    await stage("web-2");
    await run(stampPlacements(organizationId, [volumeId], "dep-1"));
    inFlight = false;
    expect(await run(discardPlacement(organizationId, null, volumeId, environment))).toMatchObject({ ok: true, placement: { staged: null, deploymentId: null } });
  });

  it("stamps only Volumes with something staged in that Environment", async () => {
    await stage("web-2");
    await run(discardPlacement(organizationId, null, volumeId, environment));
    await run(stampPlacements(organizationId, [volumeId], "dep-1"));
    expect((await run(listPlacements(organizationId)))[0]?.deploymentId).toBeNull();
  });

  it("keeps a Deploy's claim on a placement it stamped while an edit was being checked", async () => {
    await stage("web-2");
    between = { query: "volumes", run: () => run(stampPlacements(organizationId, [volumeId], "dep-1")) };
    expect(await stage("web-3")).toMatchObject({ ok: false, refusal: { code: "conflict", message: "A Deploy is moving this Volume. Wait for it to finish." } });
    expect(await run(listPlacements(organizationId))).toMatchObject([{ staged: { primary: web("web-2") }, deploymentId: "dep-1" }]);
  });

  it("keeps a Deploy's claim on a placement it stamped while a Discard was being checked", async () => {
    await stage("web-2");
    await run(stampPlacements(organizationId, [volumeId], "dep-0"));
    inFlight = false;
    // dep-0 ended: its release gives the row back, and dep-1 takes it.
    between = { query: "deployment", run: async () => {
      await harness.pool.query("update volume_placement set deployment_id = null");
      await run(stampPlacements(organizationId, [volumeId], "dep-1"));
    } };
    expect(await run(discardPlacement(organizationId, null, volumeId, environment)))
      .toMatchObject({ ok: false, refusal: { code: "conflict", message: "A Deploy is moving this Volume. Wait for it to finish." } });
    expect(await run(listPlacements(organizationId))).toMatchObject([{ staged: { primary: web("web-2") }, deploymentId: "dep-1" }]);
  });

  it("refuses edits and Discard when the Store can't say whether the Deploy still runs", async () => {
    await stage("web-2");
    await run(stampPlacements(organizationId, [volumeId], "dep-1"));
    unreadable = true;
    const unchecked = { ok: false, refusal: { code: "unavailable" } };
    expect(await stage("web-3")).toMatchObject(unchecked);
    expect(await run(discardPlacement(organizationId, null, volumeId, environment))).toMatchObject(unchecked);
    expect(await run(listPlacements(organizationId))).toMatchObject([{ staged: { primary: web("web-2") }, deploymentId: "dep-1" }]);
  });

  it("deletes only an old copy at once, never the writer or a mirror", async () => {
    await stage("web-1", ["web-2", "web-4"]);
    const members = [member("web-1", "writer"), member("web-2", "mirror"), member("web-3", "mirror"), member("web-4", "mirror")];
    const deleting = (server: string) => run(deleteOldCopy(organizationId, null, { volumeId, environment, server }, () => Effect.succeed(members)));
    for (const server of ["web-1", "web-2", "web-4"]) {
      expect(await deleting(server)).toEqual({ ok: false, refusal: { message: "Not an old copy" } });
    }
    await expect(deleting("web-3")).rejects.toThrow("no Cluster");
    expect((await harness.pool.query("select count(*)::int as runs from volume_run")).rows).toEqual([{ runs: 0 }]);
  });

  it("refuses deleting an old copy staged back onto the Volume between the check and the run", async () => {
    await stage("web-1", ["web-2"]);
    between = { query: "volumes", run: async () => { await stage("web-1", ["web-2", "web-3"]); } };
    const deleting = { volumeId, environment, kind: "delete_mirror", args: { slot: "web-3", confirmed_name: null } } as const;
    expect(await run(requestUnclaimedRun({ userId: null, organizationId }, deleting, { server: web("web-3"), point: null })))
      .toEqual({ ok: false, refusal: { message: "Not an old copy" } });
    expect(between).toBeNull();
    expect((await harness.pool.query("select count(*)::int as runs from volume_run")).rows).toEqual([{ runs: 0 }]);
  });

  describe("an old copy a staged restore reads", () => {
    const deleting = () => run(requestUnclaimedRun({ userId: null, organizationId },
      { volumeId, environment, kind: "delete_mirror", args: { slot: "web-3", confirmed_name: null } }, { server: web("web-3"), point: "10" }));
    const restoring = (guid: string, deploymentId: string | null = null) => harness.pool.query(
      "insert into volume_placement (volume_id, organization_id, environment_id, mirrors, staged, deployment_id) values ('vol3', $1, 'env-1', '[]', $2, $3)",
      [organizationId, JSON.stringify({ primary: web("web-1"), mirrors: [], deletes: [], seed: {
        sourceVolumeId: volumeId, sourceName: "data", dockerVolume: "shop-production_vol-vol3", point: { guid, name: "ployz-1", at: 1 },
      } }), deploymentId],
    );
    const runs = async () => (await harness.pool.query("select count(*)::int as runs from volume_run")).rows;
    beforeEach(async () => { await stage("web-1", ["web-2"]); });

    it.each([["staged", null], ["taken by a Deploy", "dep-1"]])("refuses deleting it while the restore is %s", async (_, deploymentId) => {
      await restoring("10", deploymentId);
      expect(await deleting()).toMatchObject({ ok: false, refusal: { message: "A restore reads this copy." } });
      expect(await runs()).toEqual([{ runs: 0 }]);
    });

    it("deletes it when the restore reads another point", async () => {
      await restoring("11");
      expect(await deleting()).toMatchObject({ ok: true });
      expect(await runs()).toEqual([{ runs: 1 }]);
    });

    it("refuses deleting it when the restore is staged while the delete waits for the Environment", async () => {
      const staging = await harness.pool.connect();
      let deleted: Promise<unknown> | undefined;
      try {
        await staging.query("select pg_advisory_lock(hashtext('volume-environment:env-1'))");
        deleted = deleting();
        await waitFor(async () => (await harness.pool.query(
          "select count(*)::int as waiting from pg_locks where locktype = 'advisory' and not granted",
        )).rows[0].waiting > 0, deleted);
        await restoring("10");
      } finally {
        await staging.query("select pg_advisory_unlock_all()");
        staging.release();
      }
      expect(await deleted).toMatchObject({ ok: false, refusal: { message: "A restore reads this copy." } });
      expect(await runs()).toEqual([{ runs: 0 }]);
    });
  });

  it("refuses deleting an old copy while a Deploy holds the Volume", async () => {
    await stage("web-1", ["web-2"]);
    await run(stampPlacements(organizationId, [volumeId], "dep-1"));
    const members = [member("web-1", "writer"), member("web-2", "mirror"), member("web-3", "mirror")];
    expect(await run(deleteOldCopy(organizationId, null, { volumeId, environment, server: "web-3" }, () => Effect.succeed(members))))
      .toEqual({ ok: false, refusal: { message: "A Deploy is moving this Volume. Wait for it to finish." } });
  });

  it("says a Deploy is moving the Volume before it says a copy the Deploy takes is not old", async () => {
    await stage("web-3", ["web-2"]);
    await run(stampPlacements(organizationId, [volumeId], "dep-1"));
    const members = [member("web-1", "writer"), member("web-2", "mirror"), member("web-3", "mirror")];
    expect(await run(deleteOldCopy(organizationId, null, { volumeId, environment, server: "web-3" }, () => Effect.succeed(members))))
      .toEqual({ ok: false, refusal: { message: "A Deploy is moving this Volume. Wait for it to finish." } });
  });

  it("requests a run once the Deploy that stamped the Volume has ended, as staging does, and not while it runs", async () => {
    await stage("web-1", ["web-2"]);
    await run(stampPlacements(organizationId, [volumeId], "dep-1"));
    const sync = () => run(requestUnclaimedRun({ userId: null, organizationId }, { volumeId, environment, kind: "sync", args: { full: false } }));
    expect(await sync()).toMatchObject({ ok: false, refusal: { message: "A Deploy is moving this Volume. Wait for it to finish." } });
    inFlight = false;
    expect(await sync()).toMatchObject({ ok: true });
    expect((await harness.pool.query("select count(*)::int as runs from volume_run")).rows).toEqual([{ runs: 1 }]);
  });

  it("refuses a run when a Deploy stamps the Volume between the caller's check and the insert", async () => {
    await stage("web-1", ["web-2"]);
    between = { query: "volumes", run: () => run(stampPlacements(organizationId, [volumeId], "dep-1")) };
    const requested = await run(requestUnclaimedRun({ userId: null, organizationId }, { volumeId, environment, kind: "sync", args: { full: false } }));
    expect(requested).toMatchObject({ ok: false, refusal: { message: "A Deploy is moving this Volume. Wait for it to finish." } });
    expect((await harness.pool.query("select count(*)::int as runs from volume_run")).rows).toEqual([{ runs: 0 }]);
  });

  describe("admitting a Deploy", () => {
    const members = [member("web-1", "writer"), member("web-2", "mirror")];
    let writes: ConfigCommand[];
    // Runs while the Store writes: an edit staged after the check, before the stamp.
    let during: (() => Promise<void>) | null;
    beforeEach(() => { writes = []; during = null; });
    const admitted = asTestDouble<ConfigCommitted>()({ written: "deployment", id: "dep-1", environment_id: "env-1", status: "admitted", checks: [] });
    const write = (command: ConfigCommand) => Effect.promise(async () => {
      writes.push(command);
      await during?.();
      return admitted;
    });
    const admit = (accept: string[], version: string | null = "v1"): ConfigCommand =>
      ({ command: "admit", admit: "deploy", id: "dep-1", environment, services: [], version, accept_volume_loss: accept });
    const admission = (command: ConfigCommand, inspect: () => Effect.Effect<Member[], Error> = () => Effect.succeed(members)) =>
      run(writeAdmitted(organizationId, command, write, inspect).pipe(
        Effect.map(() => ({ ok: true as const, command: writes.at(-1) })),
        Effect.catchTag("StoreRefused", (error) => Effect.succeed({ ok: false as const, refusal: error.refusal })),
      ));
    const holder = async () => (await run(listPlacements(organizationId)))[0]?.deploymentId;
    /** The version a refusal binds to the loss it showed. */
    const shown = async () => {
      const refused = await admission(admit([]));
      if (refused.ok) throw new Error("expected a refusal");
      return (refused.refusal.details as { version: string }).version;
    };

    it("refuses a Deploy that deletes a copy until it accepts the Volume by name, as the Store refuses deleting one", async () => {
      await stage("web-1");
      expect(await admission(admit([]))).toEqual({ ok: false, refusal: {
        code: "confirmation_required",
        message: "Deploying loses data of data. Accept each by name",
        details: { volumes: [{ name: "data", deletes: [{ machine_id: web("web-2") }], loss: "data" }], accept: ["data"], version: expect.stringMatching(/^v1#loss-/) },
      } });
      expect(await admission(admit(["other"], null))).toMatchObject({ ok: false, refusal: { details: { accept: ["other", "data"], version: expect.stringMatching(/^diff-7#loss-/) } } });
      expect(writes).toEqual([]);
      expect(await holder()).toBeNull();
      expect(await admission(admit(["data", "other"]))).toMatchObject({ ok: false, refusal: { code: "confirmation_required" } });
      expect(await admission(admit(["data", "other"], await shown()))).toEqual({ ok: true, command: admit(["other"]) });
      expect(await holder()).toBe("dep-1");
    });

    it("refuses an acceptance of a loss the Volume no longer has, and stamps nothing", async () => {
      const old = () => Effect.succeed([...members, member("web-3", "mirror")]);
      await stage("web-1");
      const shown = await admission(admit([]), old);
      if (shown.ok) throw new Error("expected a refusal");
      const { accept, version } = shown.refusal.details as { accept: string[]; version: string };
      await run(stagePlacement(organizationId, { environment, volumeId, staged: { primary: web("web-1"), mirrors: [], deletes: [web("web-3")] }, mirrors: [web("web-2")] }));
      expect(await admission(admit(accept, version), old)).toMatchObject({ ok: false, refusal: {
        code: "confirmation_required",
        details: { volumes: [{ name: "data", deletes: [{ machine_id: web("web-3") }, { machine_id: web("web-2") }], loss: "data" }] },
      } });
      expect(writes).toEqual([]);
      expect(await holder()).toBeNull();
    });

    it("leaves the name with the Store when the Deploy deletes the Volume there too", async () => {
      await stage("web-1");
      deleting = true;
      expect(await admission(admit(["data"], await shown()))).toEqual({ ok: true, command: admit(["data"]) });
    });

    it("refuses while the Servers can't be asked about a copy", async () => {
      await stage("web-1");
      expect(await admission(admit(["data"]), () => Effect.fail(new Error("down")))).toMatchObject({ ok: false, refusal: { code: "unavailable" } });
      expect(writes).toEqual([]);
    });

    it("passes a Deploy that keeps every copy, and any other command, through untouched", async () => {
      await stage("web-2");
      expect(await admission(admit([]))).toEqual({ ok: true, command: admit([]) });
      const publish: ConfigCommand = { command: "publish", environment, version: null, accept_volume_loss: [] };
      expect(await admission(publish, () => Effect.die("never inspected"))).toEqual({ ok: true, command: publish });
    });

    it.each<[string, ConfigCommand]>([
      ["a Redeploy", { command: "admit", admit: "retry", id: "dep-1", deployment: "dep-0" }],
      ["a shut down", { command: "admit", admit: "shut_down", id: "dep-1", environment, version: null, accept_volume_loss: [] }],
    ])("leaves staged copies to the next Deploy on %s, which ships nothing staged", async (_, command) => {
      await stage("web-1");
      expect(await admission(command, () => Effect.die("never inspected"))).toEqual({ ok: true, command });
      expect(await holder()).toBeNull();
    });

    it("waits while a restore is being staged in its Environment, then leaves a gone one staged", async () => {
      const seed = { sourceVolumeId: volumeId, sourceName: "data", dockerVolume: "shop-production_vol-vol3", point: { guid: "11", name: "ployz-1", at: 1 } };
      await harness.pool.query(
        "insert into volume_placement (volume_id, organization_id, environment_id, mirrors, staged) values ('vol3', $1, 'env-1', '[]', $2)",
        [organizationId, JSON.stringify({ primary: web("web-1"), mirrors: [], deletes: [], seed })],
      );
      const staging = await harness.pool.connect();
      let admitted: Promise<unknown> | undefined;
      try {
        await staging.query("select pg_advisory_lock(hashtext('volume-environment:env-1'))");
        admitted = admission(admit([]));
        await waitFor(async () => (await harness.pool.query(
          "select count(*)::int as waiting from pg_locks where locktype = 'advisory' and not granted",
        )).rows[0].waiting > 0, admitted);
        expect(writes).toEqual([]);
      } finally {
        await staging.query("select pg_advisory_unlock_all()");
        staging.release();
      }
      expect(await admitted).toEqual({ ok: true, command: admit([]) });
      expect((await harness.pool.query("select deployment_id from volume_placement where volume_id = 'vol3'")).rows).toEqual([{ deployment_id: null }]);
    });

    it("lets another Environment stage while a Deploy is admitted", async () => {
      await stage("web-1");
      const staging = { project: "shop", environment: "staging" };
      const restage = (primary: string) =>
        run(stagePlacement(organizationId, { environment: staging, volumeId: "vol2", staged: { primary: web(primary), mirrors: [], deletes: [] }, mirrors: [] }));
      await restage("web-1");
      let edit: Promise<unknown> | undefined;
      let meanwhile: unknown;
      during = async () => {
        edit = restage("web-2");
        await waitFor(async () => (await harness.pool.query(
          "select count(*)::int as waiting from pg_stat_activity where wait_event_type = 'Lock'",
        )).rows[0].waiting > 0, edit);
        meanwhile = await Promise.race([edit, Promise.resolve("blocked")]);
      };
      const version = await shown();
      expect(await admission(admit(["data"], version))).toEqual({ ok: true, command: admit([]) });
      await edit;
      expect(meanwhile).toMatchObject({ ok: true, placement: { environmentId: "env-2", staged: { primary: web("web-2") }, deploymentId: null } });
    });

    it("leaves no restore check holding the Environment once it answers", async () => {
      const held = await run(Effect.gen(function* () {
        const database = yield* Database;
        return yield* database.transaction(Effect.gen(function* () {
          yield* restoreSettled("env-1");
          return yield* Effect.promise(async () => (await harness.pool.query(
            `select count(*)::int as held from pg_locks where locktype = 'advisory'
              and database = (select oid from pg_database where datname = current_database())
              and objid::bigint = hashtext('volume-environment:env-1')::bigint & 4294967295`,
          )).rows[0].held);
        }));
      }));
      expect(held).toBe(0);
    });

    it("leaves a Volume another Deploy took to that Deploy, neither confirming its loss nor taking it", async () => {
      await stage("web-1");
      await run(stampPlacements(organizationId, [volumeId], "dep-0"));
      expect(await admission(admit([]))).toEqual({ ok: true, command: admit([]) });
      expect(await holder()).toBe("dep-0");
    });

    it("refuses an edit staged between the check and the stamp, and stamps what it checked", async () => {
      await stage("web-1", ["web-2"]);
      let edit: Promise<unknown> | undefined;
      during = async () => {
        edit = stage("web-1");
        await waitFor(async () => (await harness.pool.query(
          "select count(*)::int as waiting from pg_stat_activity where wait_event_type = 'Lock'",
        )).rows[0].waiting > 0, edit);
      };
      expect(await admission(admit([]))).toEqual({ ok: true, command: admit([]) });
      expect(await edit).toMatchObject({ ok: false, refusal: { message: "A Deploy is moving this Volume. Wait for it to finish." } });
      expect(await run(listPlacements(organizationId))).toMatchObject([{ staged: { mirrors: [web("web-2")] }, deploymentId: "dep-1" }]);
    });
  });
});
