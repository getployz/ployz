import { createRequire } from "node:module";
import type * as PloyzSdk from "@ployz/sdk";
import type { ConfigStore, MachineId } from "@ployz/sdk";
import { ConfigProvider, Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { GithubApi } from "#/modules/github/github-observation.api";
import { InngestClient } from "#/modules/inngest/client";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { deleteOldCopy, discardPlacement, listPlacements, requestUnclaimedRun, stagePlacement, stampPlacements } from "#/modules/volume-placement/volume-placement.server";
import { member } from "#/modules/volume-run/volume-run.test-fixture";
import { AppConfig } from "#/server/config.server";
import type { Database } from "#/server/database.server";
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

describe("volume placement", () => {
  let harness: PostgresTestHarness;
  let inFlight: boolean;
  let unreadable: boolean;
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
        read: async (_: string, query: { query: string }) => {
          if (between?.query === query.query) {
            const { run: admit } = between;
            between = null;
            await admit();
          }
          if (query.query === "deployment" && unreadable) throw new RpcError({ code: "unavailable", message: "Store down", details: null });
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
    unreadable = false;
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
      volumeId, environmentId: "env-1", mirrors: [web("web-2")], staged: { primary: web("web-3"), mirrors: [], deletes: [] }, deploymentId: null,
    }]);
  });

  it("hands the staged placement to the Deploy, and refuses Discard and new edits while it runs", async () => {
    await stage("web-2");
    await run(stampPlacements(organizationId, "env-1", "dep-1"));
    expect((await run(listPlacements(organizationId)))[0]?.deploymentId).toBe("dep-1");
    expect(await run(discardPlacement(organizationId, null, volumeId, environment))).toMatchObject({ ok: false, refusal: { code: "conflict" } });
    expect(await stage("web-3")).toMatchObject({ ok: false });
    expect((await run(listPlacements(organizationId)))[0]?.staged?.primary).toBe(web("web-2"));
  });

  it("discards once the Deploy has ended", async () => {
    await stage("web-2");
    await run(stampPlacements(organizationId, "env-1", "dep-1"));
    inFlight = false;
    expect(await run(discardPlacement(organizationId, null, volumeId, environment))).toMatchObject({ ok: true, placement: { staged: null, deploymentId: null } });
  });

  it("stamps only Volumes with something staged in that Environment", async () => {
    await stage("web-2");
    await run(discardPlacement(organizationId, null, volumeId, environment));
    await run(stampPlacements(organizationId, "env-1", "dep-1"));
    expect((await run(listPlacements(organizationId)))[0]?.deploymentId).toBeNull();
  });

  it("keeps a Deploy's claim on a placement it stamped while an edit was being checked", async () => {
    await stage("web-2");
    between = { query: "volumes", run: () => run(stampPlacements(organizationId, "env-1", "dep-1")) };
    expect(await stage("web-3")).toMatchObject({ ok: false, refusal: { code: "conflict", message: "A Deploy is moving this Volume. Wait for it to finish." } });
    expect(await run(listPlacements(organizationId))).toMatchObject([{ staged: { primary: web("web-2") }, deploymentId: "dep-1" }]);
  });

  it("keeps a Deploy's claim on a placement it stamped while a Discard was being checked", async () => {
    await stage("web-2");
    await run(stampPlacements(organizationId, "env-1", "dep-0"));
    inFlight = false;
    between = { query: "deployment", run: () => run(stampPlacements(organizationId, "env-1", "dep-1")) };
    expect(await run(discardPlacement(organizationId, null, volumeId, environment)))
      .toMatchObject({ ok: false, refusal: { code: "conflict", message: "A Deploy is moving this Volume. Wait for it to finish." } });
    expect(await run(listPlacements(organizationId))).toMatchObject([{ staged: { primary: web("web-2") }, deploymentId: "dep-1" }]);
  });

  it("refuses edits and Discard when the Store can't say whether the Deploy still runs", async () => {
    await stage("web-2");
    await run(stampPlacements(organizationId, "env-1", "dep-1"));
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

  it("refuses deleting an old copy while a Deploy holds the Volume", async () => {
    await stage("web-1", ["web-2"]);
    await run(stampPlacements(organizationId, "env-1", "dep-1"));
    const members = [member("web-1", "writer"), member("web-2", "mirror"), member("web-3", "mirror")];
    expect(await run(deleteOldCopy(organizationId, null, { volumeId, environment, server: "web-3" }, () => Effect.succeed(members))))
      .toEqual({ ok: false, refusal: { message: "A Deploy is moving this Volume. Wait for it to finish." } });
  });

  it("requests a run once the Deploy that stamped the Volume has ended, as staging does, and not while it runs", async () => {
    await stage("web-1", ["web-2"]);
    await run(stampPlacements(organizationId, "env-1", "dep-1"));
    const sync = () => run(requestUnclaimedRun({ userId: null, organizationId }, { volumeId, environment, kind: "sync", args: { full: false } }));
    expect(await sync()).toMatchObject({ ok: false, refusal: { message: "A Deploy is moving this Volume. Wait for it to finish." } });
    inFlight = false;
    expect(await sync()).toMatchObject({ ok: true });
    expect((await harness.pool.query("select count(*)::int as runs from volume_run")).rows).toEqual([{ runs: 1 }]);
  });

  it("refuses a run when a Deploy stamps the Volume between the caller's check and the insert", async () => {
    await stage("web-1", ["web-2"]);
    between = { query: "volumes", run: () => run(stampPlacements(organizationId, "env-1", "dep-1")) };
    const requested = await run(requestUnclaimedRun({ userId: null, organizationId }, { volumeId, environment, kind: "sync", args: { full: false } }));
    expect(requested).toMatchObject({ ok: false, refusal: { message: "A Deploy is moving this Volume. Wait for it to finish." } });
    expect((await harness.pool.query("select count(*)::int as runs from volume_run")).rows).toEqual([{ runs: 0 }]);
  });
});
