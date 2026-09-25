import { testConfigEnvironment } from "#/test/config-environment";
import { createPrivateKey, X509Certificate } from "node:crypto";
import type { MachineId, PublishCertificateMaterialRequest, RuntimeWatchView } from "@ployz/sdk";
import { InngestTestEngine } from "@inngest/test";
import { ConfigProvider, Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { reserveClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import { type FakeHostedDns, startFakeHostedDns } from "#/modules/cluster-domain/hosted-dns.test-fixture";
import { createScheduleClusterDomainSync, createSyncClusterDomain } from "#/modules/cluster-domain/sync.inngest";
import {
  type PostgresTestHarness,
  startPostgresTestHarness,
} from "#/test/postgres";
import { asTestDouble } from "#/lib/test-double";
import { InngestClient } from "#/modules/inngest/client";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import {
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { AppConfig } from "#/server/config.server";
import type { Database, ReportingDatabase } from "#/server/database.server";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";

const organizationId = "00000000-0000-4000-8000-000000000b01";
const otherOrganizationId = "00000000-0000-4000-8000-000000000b02";
const encryption = makeSecretEncryption("cluster-domain-sync-test-encryption-secret");
const machine = (id: string, public_ip: string | null, accepts_ingress = true) =>
  runtimeWatchMachineObservationFixture({ machine: runtimeWatchMachineFixture(id.repeat(32).slice(0, 32), id, { public_ip, accepts_ingress }) });
const idOf = (id: string) => id.repeat(32).slice(0, 32) as MachineId;
const inngest = new Inngest({ id: "cluster-domain-sync-test" });
vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

describe("sync-cluster-domain", () => {
  let harness: PostgresTestHarness;
  let hostedDns: FakeHostedDns;
  /** The runtime frame the stubbed session returns; null means no Cluster is paired. */
  let frame: RuntimeWatchView | null;
  /** A paired Cluster that cannot be reached. */
  let offline: boolean;
  /** Probe answers by address; an absent address refuses the connection. */
  const verify = new Map<string, string>();
  /** Certificate Material the stubbed session was asked to publish. */
  const publishedMaterial: PublishCertificateMaterialRequest[] = [];

  const runEffect = makeInngestEffectRunner(<A, E>(
    operation: Effect.Effect<A, E, Database | ReportingDatabase | AppConfig | SecretEncryption | OrganizationRuntime | InngestClient>,
  ) => {
    const config = AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({
      env: {
        ...testConfigEnvironment(),
        DATABASE_URL: harness.databaseUrl,
        PLOYZ_HOSTED_DNS_URL: hostedDns.url,
      },
    }))));
    return harness.runEffect(operation.pipe(
      Effect.provideService(SecretEncryption, encryption),
      Effect.provideService(InngestClient, inngest),
      Effect.provideService(OrganizationRuntime, {
        cancel: () => Effect.void,
        open: () => Effect.succeed(offline
          ? { status: "unreachable" as const, error: null }
          : frame === null
          ? { status: "no_connection" as const }
          // The sync only reads the first runtime frame.
          : { status: "connected" as const, connected: asTestDouble<PloyzSession>()({
            watchFirstFrame: () => Effect.succeed(frame),
            publishCertificateMaterial: (request: PublishCertificateMaterialRequest) => Effect.sync(() => { publishedMaterial.push(request); }),
          }) }),
      }),
      Effect.provide(config),
    ));
  }) as typeof runInngestEffect;

  const sync = (id = organizationId) => new InngestTestEngine({
    function: createSyncClusterDomain(new Inngest({ id: "test" }), runEffect),
    events: [{ name: "cluster-domain/sync.requested", data: { organizationId: id } }],
  }).execute();
  const reserve = (id = organizationId) => runEffect(reserveClusterDomain(id));
  const row = async () => (await harness.pool.query(
    `select name, records_synced_at, lease_renewed_at, traffic, checked_at,
            encrypted_certificate_private_key, certificate_chain, certificate_not_after from organization_cluster_domain`,
  )).rows[0] as {
    name: string;
    records_synced_at: Date | null;
    lease_renewed_at: Date;
    traffic: unknown;
    checked_at: Date | null;
    encrypted_certificate_private_key: Parameters<typeof encryption.decrypt>[0] | null;
    certificate_chain: string | null;
    certificate_not_after: Date | null;
  } | undefined;
  const storedCertificate = async () => {
    const stored = await row();
    const key = stored?.encrypted_certificate_private_key;
    const chain = stored?.certificate_chain;
    const notAfter = stored?.certificate_not_after;
    if (!key || !chain || !notAfter) throw new Error("No certificate is stored.");
    return { encryptedKey: key, privateKeyPem: encryption.decrypt(key), chain, notAfter };
  };
  const calls = () => hostedDns.requests.map(({ method, path }) => `${method} ${path}`);

  beforeAll(async () => {
    [harness, hostedDns] = await Promise.all([startPostgresTestHarness(), startFakeHostedDns()]);
    const realFetch = globalThis.fetch;
    vi.spyOn(globalThis, "fetch").mockImplementation((input, init) => {
      const url = new URL(input instanceof Request ? input.url : input);
      if (url.pathname !== "/.ployz-verify") return realFetch(input, init);
      const body = verify.get(url.hostname.replace(/^\[|\]$/gu, ""));
      return body === undefined ? Promise.reject(new TypeError("fetch failed")) : Promise.resolve(new Response(body));
    });
  }, 60_000);

  afterAll(async () => {
    vi.restoreAllMocks();
    await Promise.all([harness?.stop(), hostedDns?.close()]);
  });

  beforeEach(async () => {
    hostedDns.requests.length = 0;
    hostedDns.state.failWith = null;
    hostedDns.state.gone.clear();
    hostedDns.state.certificateFailWith = null;
    hostedDns.state.certificateDays = 90;
    verify.clear();
    publishedMaterial.length = 0;
    frame = null;
    offline = false;
    await harness.pool.query(`
      truncate table organization cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme'), ('${otherOrganizationId}', 'Other', 'other');
    `);
    await reserve();
    hostedDns.requests.length = 0;
  });

  it("skips an Organization with no reserved name and never reserves one", async () => {
    const output = await sync(otherOrganizationId);

    expect(output.result).toEqual({ organizationId: otherOrganizationId, skipped: true });
    expect(calls()).toEqual([]);
  });

  it("publishes the ingress Servers that answer, which renews the lease, and records the check", async () => {
    frame = runtimeWatchFrameFixture({
      machines: [
        machine("a", "203.0.113.1"),
        machine("b", "2001:db8::1"),
        machine("c", "198.51.100.7"),
        machine("d", "198.51.100.8", false),
        machine("e", null),
      ],
    });
    verify.set("203.0.113.1", idOf("a"));
    verify.set("2001:db8::1", `${idOf("b")}\n`);
    verify.set("198.51.100.7", idOf("x"));
    verify.set("198.51.100.8", idOf("d"));

    const output = await sync();

    expect(output.error).toBeUndefined();
    expect(output.result).toEqual({
      organizationId,
      name: "acme.ployz.test",
      observed: true,
      recordsPut: true,
      certificateIssued: true,
      certificatePublished: true,
    });
    expect(calls()).toEqual([
      "PUT /domains/acme.ployz.test/records",
      "POST /domains/acme.ployz.test/certificate",
    ]);
    expect(hostedDns.requests[0]).toMatchObject({ authorization: "Bearer token-1", body: { a: ["203.0.113.1"], aaaa: ["2001:db8::1"] } });
    expect(await row()).toMatchObject({
      records_synced_at: expect.any(Date),
      traffic: { kind: "probed", unreachable: [{ machineId: idOf("c"), address: "198.51.100.7" }] },
      checked_at: expect.any(Date),
    });
  });

  it.each([
    ["no Cluster is paired", null, "no_servers"],
    ["the Cluster has no Servers", [], "no_servers"],
    ["no ingress Server has a public IP", [machine("a", null), machine("b", "203.0.113.2", false)], "no_public_ip"],
  ] as const)("records why there is no traffic when %s, and still renews the lease", async (_, machines, kind) => {
    frame = machines === null ? null : runtimeWatchFrameFixture({ machines: [...machines] });

    const output = await sync();

    expect(output.result).toMatchObject({ observed: true, recordsPut: false });
    expect(calls()).toEqual(["POST /domains/acme.ployz.test/lease", "POST /domains/acme.ployz.test/certificate"]);
    expect(await row()).toMatchObject({ records_synced_at: null, traffic: { kind }, checked_at: expect.any(Date) });
  });

  it("clears the last finding while the Cluster is offline, and still records the check", async () => {
    frame = runtimeWatchFrameFixture({ machines: [] });
    await sync();
    const before = await row();
    offline = true;

    const output = await sync();

    expect(output.result).toMatchObject({ observed: false, recordsPut: false });
    const after = await row();
    expect(before).toMatchObject({ traffic: { kind: "no_servers" } });
    expect(after).toMatchObject({ traffic: null });
    expect(after?.checked_at?.getTime()).toBeGreaterThan(before?.checked_at?.getTime() ?? Infinity);
  });

  it("writes no records with nothing reachable, and still renews the lease", async () => {
    frame = runtimeWatchFrameFixture({ machines: [machine("a", "203.0.113.1")] });
    const output = await sync();
    expect(output.result).toMatchObject({ observed: true, recordsPut: false });

    expect(calls()).toEqual([
      "POST /domains/acme.ployz.test/lease",
      "POST /domains/acme.ployz.test/certificate",
    ]);
    expect(await row()).toMatchObject({
      records_synced_at: null,
      traffic: { kind: "probed", unreachable: [{ machineId: idOf("a"), address: "203.0.113.1" }] },
    });
  });

  it.each([401, 404, 410])("keeps the name and fails without retrying when Hosted DNS answers %i", async (status) => {
    await sync();
    hostedDns.requests.length = 0;
    hostedDns.state.gone.set("acme.ployz.test", status);

    const output = await sync();

    expect(output.error).toEqual(expect.objectContaining({ stack: expect.stringContaining("NonRetriableError") }));
    expect(calls()).toEqual(["POST /domains/acme.ployz.test/lease"]);
    expect(await row()).toMatchObject({ name: "acme.ployz.test" });
  });

  it("issues the wildcard once, stores its key encrypted, and republishes it to the Cluster on every sync", async () => {
    frame = runtimeWatchFrameFixture({ machines: [] });

    const first = await sync();
    const stored = await storedCertificate();
    const second = await sync();

    expect(first.result).toMatchObject({ certificateIssued: true, certificatePublished: true });
    expect(second.result).toMatchObject({ certificateIssued: false, certificatePublished: true });
    expect(calls().filter((call) => call.endsWith("/certificate"))).toHaveLength(1);
    const request = hostedDns.requests.find(({ path }) => path.endsWith("/certificate"));
    expect(request).toMatchObject({ authorization: "Bearer token-1", body: { csr: expect.stringContaining("BEGIN CERTIFICATE REQUEST") } });

    const leaf = new X509Certificate(stored.chain);
    expect(leaf.checkPrivateKey(createPrivateKey(stored.privateKeyPem))).toBe(true);
    expect(stored.notAfter).toEqual(new Date(leaf.validTo));
    expect(JSON.stringify(stored.encryptedKey)).not.toContain("PRIVATE KEY");
    const material = {
      hostname: "*.acme.ployz.test",
      change: { action: "set", certificate_chain_pem: stored.chain, private_key_pem: stored.privateKeyPem },
    };
    expect(publishedMaterial).toEqual([material, material]);
  });

  it("replaces a wildcard with under 30 days left, and keeps it when Hosted DNS fails", async () => {
    frame = runtimeWatchFrameFixture({ machines: [] });
    hostedDns.state.certificateDays = 20;
    await sync();
    const expiring = await storedCertificate();

    hostedDns.state.certificateFailWith = 429;
    const failed = await sync();
    expect(failed.error).toBeUndefined();
    expect(failed.result).toMatchObject({ certificateIssued: false, certificatePublished: true });
    expect((await storedCertificate()).chain).toBe(expiring.chain);
    expect(publishedMaterial.at(-1)?.change).toMatchObject({ certificate_chain_pem: expiring.chain });

    hostedDns.state.certificateFailWith = null;
    hostedDns.state.certificateDays = 90;
    const renewed = await sync();
    expect(renewed.result).toMatchObject({ certificateIssued: true, certificatePublished: true });
    expect((await storedCertificate()).notAfter.getTime()).toBeGreaterThan(expiring.notAfter.getTime());
    expect(calls().filter((call) => call.endsWith("/certificate"))).toHaveLength(3);
  });

  it("the hourly cron requests a sync for every Organization with a Cluster Domain, paired or not", async () => {
    await harness.pool.query(`
      insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_public_key, founder_claim_machine_id, founder_machine_id)
      values ('${otherOrganizationId}', '{}', null, '${idOf("b")}', '${idOf("b")}');
    `);
    const fn = createScheduleClusterDomainSync(new Inngest({ id: "test" }), runEffect);
    expect(fn.opts.triggers).toEqual([{ cron: "TZ=UTC 0 * * * *" }]);

    const output = await new InngestTestEngine({
      function: fn,
      steps: [{ id: "request-cluster-domain-syncs", handler: () => ({ ids: ["event-1"] }) }],
    }).execute();

    expect(output.result).toEqual({ organizationCount: 1 });
    expect(output.ctx.step.sendEvent).toHaveBeenCalledWith("request-cluster-domain-syncs", [
      { name: "cluster-domain/sync.requested", data: { organizationId } },
    ]);
  });
});
