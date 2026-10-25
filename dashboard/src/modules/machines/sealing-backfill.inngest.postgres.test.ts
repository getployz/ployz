import { testConfigEnvironment } from "#/test/config-environment";
import type { BackfillSealingKeyReport, BackfillSealingKeyRequest, MachineId } from "@ployz/sdk";
import { InngestTestEngine } from "@inngest/test";
import { ConfigProvider, Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { openCloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import { createEnsureClusterSealingKey } from "#/modules/machines/sealing-backfill.inngest";
import { chooseSealingKeyGenerator, recordSealingPublicKey } from "#/modules/machines/sealing-key.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { AppConfig } from "#/server/config.server";
import type { Database, ReportingDatabase } from "#/server/database.server";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";

const organizationId = "00000000-0000-4000-8000-000000000c01";
const encryption = makeSecretEncryption("sealing-backfill-test-encryption-secret");
const pairingSecret = "sealing-backfill-test-pairing-secret";
const clusterKey = hashEnrollmentToken(pairingSecret);
const machine = (digit: string) => digit.repeat(32) as MachineId;
const sealingKey = (letter: string) => `${letter.repeat(43)}=`;
const inngest = new Inngest({ id: "sealing-backfill-test" });
const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

describe("ensure-cluster-sealing-key", () => {
  let harness: PostgresTestHarness;
  let reports: BackfillSealingKeyReport[] | null;
  let unreachable: boolean;
  const requests: BackfillSealingKeyRequest[] = [];

  const runEffect = makeInngestEffectRunner(<A, E>(
    operation: Effect.Effect<A, E, Database | ReportingDatabase | AppConfig | SecretEncryption | OrganizationRuntime | InngestClient>,
  ) => {
    const config = AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({
      env: { ...testConfigEnvironment(), DATABASE_URL: harness.databaseUrl },
    }))));
    return harness.runEffect(operation.pipe(
      Effect.provideService(SecretEncryption, encryption),
      Effect.provideService(InngestClient, inngest),
      Effect.provideService(OrganizationRuntime, {
        cancel: () => Effect.void,
        open: () => Effect.succeed(unreachable
          ? { status: "unreachable" as const, error: null }
          : reports === null
          ? { status: "no_connection" as const }
          : { status: "connected" as const, connected: asTestDouble<PloyzSession>()({
            backfillSealingKey: (request: BackfillSealingKeyRequest) => Effect.sync(() => {
              requests.push(request);
              const report = reports?.shift();
              if (report === undefined) throw new Error("The Cluster was asked for one backfill too many.");
              return report;
            }),
          }) }),
      }),
      Effect.provide(config),
    ));
  }) as typeof runInngestEffect;

  const ensure = () => new InngestTestEngine({
    function: createEnsureClusterSealingKey(new Inngest({ id: "test" }), runEffect),
    events: [{ name: "sealing-key/ensure.requested", data: { organizationId } }],
  }).execute();
  const pair = () => harness.pool.query(
    `insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_claim_machine_id, founder_machine_id)
     values ($1, $2, $3, $3)`,
    [organizationId, JSON.stringify(encryption.encrypt(pairingSecret)), machine("1")],
  );
  const recordedKeys = async () => (await harness.pool.query(
    "select public_key from cluster_sealing_key where organization_id = $1 and cluster_key = $2",
    [organizationId, clusterKey],
  )).rows.map(({ public_key }) => public_key as string);
  const generators = async () => (await harness.pool.query(
    "select machine_id from cluster_sealing_key_generator where organization_id = $1 and cluster_key = $2",
    [organizationId, clusterKey],
  )).rows.map(({ machine_id }) => machine_id as string);
  const sentEvents = (output: Awaited<ReturnType<typeof ensure>>) =>
    vi.mocked(output.ctx.step.sendEvent).mock.calls.length + send.mock.calls.length;

  beforeAll(async () => {
    harness = await startPostgresTestHarness();
    await harness.runEffect(openCloudStore(harness.databaseUrl, harness.database, "test-encryption-secret"));
  }, 60_000);

  afterAll(async () => {
    vi.restoreAllMocks();
    await harness?.stop();
  });

  beforeEach(async () => {
    reports = [];
    unreachable = false;
    requests.length = 0;
    send.mockClear();
    await harness.pool.query(`
      truncate table organization cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
    `);
    await pair();
  });

  describe("the Sealing public key recorder", () => {
    it("keeps the first key reported", async () => {
      await runEffect(recordSealingPublicKey(organizationId, clusterKey, sealingKey("A")));
      await runEffect(recordSealingPublicKey(organizationId, clusterKey, sealingKey("A")));

      expect(await recordedKeys()).toEqual([sealingKey("A")]);
    });

    it("keeps the first row when a later key differs", async () => {
      await runEffect(recordSealingPublicKey(organizationId, clusterKey, sealingKey("A")));
      await runEffect(recordSealingPublicKey(organizationId, clusterKey, sealingKey("B")));

      expect(await recordedKeys()).toEqual([sealingKey("A")]);
    });
  });

  describe("the generator row", () => {
    it("keeps the first Machine chosen and returns it to a later choice", async () => {
      expect(await runEffect(chooseSealingKeyGenerator(organizationId, clusterKey, machine("2")))).toBe(machine("2"));
      expect(await runEffect(chooseSealingKeyGenerator(organizationId, clusterKey, machine("3")))).toBe(machine("2"));

      expect(await generators()).toEqual([machine("2")]);
    });

    it("is deleted when its generator left the Cluster", async () => {
      await runEffect(chooseSealingKeyGenerator(organizationId, clusterKey, machine("2")));
      reports = [{ result: "generator_gone", generator: machine("2") }];

      const output = await ensure();

      expect(output.result).toEqual({ organizationId, result: "generator_gone", awaitsOperator: false });
      expect(requests).toEqual([{ expected: null, generator: machine("2") }]);
      expect(await generators()).toEqual([]);
    });

    it("stays when the report names another Machine as gone", async () => {
      await runEffect(chooseSealingKeyGenerator(organizationId, clusterKey, machine("2")));
      reports = [{ result: "generator_gone", generator: machine("3") }];

      await ensure();

      expect(await generators()).toEqual([machine("2")]);
    });
  });

  describe("report handling", () => {
    it("records a distributed key and passes it as expected next time", async () => {
      reports = [{
        result: "distributed",
        public_key: sealingKey("A"),
        machines: [
          { machine: machine("1"), result: "held" },
          { machine: machine("2"), result: "failed", error: { code: "internal", message: "disk full", details: null } },
        ],
      }];

      const output = await ensure();
      reports = [{ result: "distributed", public_key: sealingKey("A"), machines: [] }];
      await ensure();

      expect(output.result).toEqual({ organizationId, result: "distributed", awaitsOperator: false });
      expect(await recordedKeys()).toEqual([sealingKey("A")]);
      expect(requests).toEqual([{ expected: null, generator: null }, { expected: sealingKey("A"), generator: null }]);
    });

    it("chooses the candidate and reruns once with it as generator", async () => {
      reports = [
        { result: "needs_generator", candidate: machine("2") },
        { result: "distributed", public_key: sealingKey("A"), machines: [{ machine: machine("2"), result: "generated" }] },
      ];

      const output = await ensure();

      expect(output.result).toEqual({ organizationId, result: "distributed", awaitsOperator: false });
      expect(requests).toEqual([{ expected: null, generator: null }, { expected: null, generator: machine("2") }]);
      expect(await generators()).toEqual([machine("2")]);
      expect(await recordedKeys()).toEqual([sealingKey("A")]);
    });

    it("reruns with the generator already chosen rather than a new candidate", async () => {
      await runEffect(chooseSealingKeyGenerator(organizationId, clusterKey, machine("2")));
      reports = [
        { result: "needs_generator", candidate: machine("3") },
        { result: "waiting", generator: machine("2") },
      ];

      const output = await ensure();

      expect(output.result).toEqual({ organizationId, result: "waiting", awaitsOperator: false });
      expect(requests[1]).toEqual({ expected: null, generator: machine("2") });
      expect(await generators()).toEqual([machine("2")]);
    });

    it("stops after the rerun when it still needs a generator", async () => {
      reports = [
        { result: "needs_generator", candidate: machine("2") },
        { result: "needs_generator", candidate: machine("2") },
      ];

      const output = await ensure();

      expect(output.error).toBeUndefined();
      expect(requests).toHaveLength(2);
    });

    it.each<BackfillSealingKeyReport>([
      { result: "conflicting", keys: [sealingKey("A"), sealingKey("B")] },
      { result: "generation_failed", generator: machine("2"), error: { code: "internal", message: "no entropy", details: null } },
      { result: "no_candidate" },
      { result: "waiting", generator: machine("2") },
    ])("logs $result and stops without recording anything", async (report) => {
      reports = [report];

      const output = await ensure();

      expect(output.result).toEqual({ organizationId, result: report.result, awaitsOperator: false });
      expect(requests).toHaveLength(1);
      expect(await recordedKeys()).toEqual([]);
    });

    it("stops for the operator on awaits_recovery after a generator was chosen, sending nothing and keeping the row", async () => {
      reports = [
        { result: "needs_generator", candidate: machine("2") },
        { result: "awaits_recovery", machine: machine("2") },
      ];

      const output = await ensure();

      expect(output.error).toBeUndefined();
      expect(output.result).toEqual({ organizationId, result: "awaits_recovery", awaitsOperator: true });
      expect(requests).toHaveLength(2);
      expect(sentEvents(output)).toBe(0);
      expect(await generators()).toEqual([machine("2")]);
    });

    it("stops for the operator when a Server awaits recovery, electing no generator and sending nothing", async () => {
      reports = [{ result: "awaits_recovery", machine: machine("3") }];

      const output = await ensure();

      expect(output.error).toBeUndefined();
      expect(output.result).toEqual({ organizationId, result: "awaits_recovery", awaitsOperator: true });
      expect(requests).toEqual([{ expected: null, generator: null }]);
      expect(sentEvents(output)).toBe(0);
      expect(await generators()).toEqual([]);
    });

    it("stops for the operator on no_holder with one backfill", async () => {
      await runEffect(recordSealingPublicKey(organizationId, clusterKey, sealingKey("A")));
      reports = [{ result: "no_holder", expected: sealingKey("A") }];

      const output = await ensure();

      expect(output.error).toBeUndefined();
      expect(output.result).toEqual({ organizationId, result: "no_holder", awaitsOperator: true });
      expect(requests).toEqual([{ expected: sealingKey("A"), generator: null }]);
      expect(sentEvents(output)).toBe(0);
    });

    it("expects the Organization's Sealing Key for a Cluster that reported none, so no Server generates another", async () => {
      await harness.pool.query(
        "insert into organization_sealing_key (organization_id, public_key) values ($1, $2)",
        [organizationId, sealingKey("A")],
      );
      reports = [{ result: "no_holder", expected: sealingKey("A") }];

      const output = await ensure();

      expect(output.result).toEqual({ organizationId, result: "no_holder", awaitsOperator: true });
      expect(requests).toEqual([{ expected: sealingKey("A"), generator: null }]);
      expect(await generators()).toEqual([]);
    });
  });

  it("skips an Organization without a founded Cluster", async () => {
    await harness.pool.query("delete from organization_pairing");

    expect((await ensure()).result).toEqual({ organizationId, skipped: true });
    expect(requests).toEqual([]);
  });

  it("skips when no Cluster answers for the Organization", async () => {
    reports = null;

    expect((await ensure()).result).toEqual({ organizationId, skipped: true });
  });

  it("fails the step, so Inngest retries it, when the Cluster cannot be reached", async () => {
    unreachable = true;

    expect((await ensure()).error).toBeDefined();
  });
});
