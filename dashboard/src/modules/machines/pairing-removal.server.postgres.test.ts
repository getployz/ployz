import { loadOrganizationConnections } from "#/modules/machines/connections.server";
import type { Client, ConnectOptions } from "@ployz/sdk";
import { Layer, ManagedRuntime } from "effect";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { startGithubPostgresTestHarness, type GithubPostgresTestHarness } from "#/modules/github/github-ingestion.postgres-test-harness";
import { hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import { disableOrganizationPairing, loadTeardownConnections, revokeOrganizationPairing } from "#/modules/machines/pairing-removal.server";
import { OrganizationRuntimeLive } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";

const organizationId = "00000000-0000-4000-8000-000000008811";
const machineId = "00000000000000000000000000008811";
const otherMachineId = "00000000000000000000000000008812";
const pairing = "ppair_fixture_removal_generation";
const capability = "ployz1:fixture-removal";
const encryption = makeSecretEncryption("fixture-removal-encryption-1234567890");

describe("protected pairing removal", () => {
  let harness: GithubPostgresTestHarness;
  beforeAll(async () => { harness = await startGithubPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness.stop(); });
  beforeEach(async () => {
    await harness.pool.query("truncate organization cascade");
    await harness.pool.query("insert into organization (id,name,slug) values ($1,'Removal','removal')", [organizationId]);
    await seedPairing();
  });

  async function seedPairing(secret = pairing, expected = capability, preferred = true) {
    await harness.pool.query(`insert into organization_pairing
      (organization_id, encrypted_pairing_secret, founder_public_key, founder_claim_machine_id)
      values ($1,$2,'founder-key',$3)`, [organizationId, encryption.encrypt(secret), machineId]);
    await harness.pool.query(`insert into organization_machine
      (organization_id,machine_id,cluster_key,encrypted_capability,is_dial_entry)
      values ($1,$2,$3,$4,$5)`, [organizationId, machineId, hashEnrollmentToken(secret), encryption.encrypt(expected), preferred]);
  }

  function fixture() {
    // The authenticated endpoint distinguishes a cleared Management Client from a replaced client key.
    const endpoint = { paired: true, online: true, loseAck: false, replaced: false };
    const dialed: ConnectOptions[] = [];
    let mutations = 0;
    const ployz = makePloyzLayer({
      connect: async (options) => {
        dialed.push(options);
        const [connection] = options.connections;
        if (!connection || !("management" in connection) || connection.management !== capability || connection.machine_id !== machineId) {
          throw new Error("Endpoint unavailable");
        }
        if (!endpoint.online) throw new Error("Endpoint unavailable");
        if (endpoint.replaced) throw Object.assign(new Error("key replaced"), { code: "unauthenticated", details: null });
        if (!endpoint.paired) throw Object.assign(new Error("management client cleared"), { code: "unauthenticated", details: { management_client: "cleared" } });
        return asTestDouble<Client>()({
          clearManagementClient: async (label: string) => {
            if (label !== "cloud") throw new Error(`unexpected Management Client ${label}`);
            mutations += 1;
            endpoint.paired = false;
            if (endpoint.loseAck) throw new Error("Rotation closed the old stream");
          },
          close: async () => undefined,
        });
      },
    });
    const layer = Layer.mergeAll(ployz, Layer.succeed(Database, harness.database), Layer.succeed(SecretEncryption, encryption));
    const makeRuntime = () => ManagedRuntime.make(OrganizationRuntimeLive.pipe(Layer.provideMerge(layer)));
    return { endpoint, dialed, mutations: () => mutations, makeRuntime };
  }

  it.each([
    { status: "confirmed", machineId, encryptedExpected: encryption.encrypt("retained-secret") },
    { status: "pending", machineId, encryptedExpected: encryption.encrypt(capability), unexpectedField: true },
    { status: "unknown", machineId, encryptedExpected: encryption.encrypt(capability) },
    { status: "pending", machineId: "invalid-id", encryptedExpected: encryption.encrypt(capability) },
    { status: "pending", machineId, encryptedExpected: { version: 2, iv: "", tag: "", ciphertext: "" } },
  ])("rejects malformed persisted removal state before dialing: %j", async (endpoint) => {
    await harness.pool.query("delete from organization_machine where organization_id=$1", [organizationId]);
    await harness.pool.query("update organization_pairing set removal_started_at=now(), removal_endpoints=$2 where organization_id=$1",
      [organizationId, JSON.stringify([endpoint])]);
    const fake = fixture();
    const runtime = fake.makeRuntime();
    try {
      await expect(runtime.runPromise(revokeOrganizationPairing(organizationId)))
        .rejects.toMatchObject({ _tag: "PairingRemovalStateInvalid" });
      await expect(runtime.runPromise(loadTeardownConnections(organizationId)))
        .rejects.toMatchObject({ _tag: "PairingRemovalStateInvalid" });
      expect(fake.dialed).toEqual([]);
      expect(fake.mutations()).toBe(0);
      const saved = await harness.pool.query("select removal_endpoints from organization_pairing where organization_id=$1", [organizationId]);
      expect(saved.rows[0].removal_endpoints).toEqual([endpoint]);
    } finally { await runtime.dispose(); }
  });

  it("disables ordinary access, moves encrypted credentials, and retains the founding claim offline", async () => {
    await expect(harness.pool.query("update organization_pairing set removal_started_at=now() where organization_id=$1", [organizationId]))
      .rejects.toMatchObject({ constraint: "organization_pairing_removal_shape_check" });
    const fake = fixture();
    fake.endpoint.online = false;
    const runtime = fake.makeRuntime();
    try {
      const result = await runtime.runPromise(revokeOrganizationPairing(organizationId));
      expect(result).toEqual({ confirmed: false, endpoints: [{ machineId, status: "unconfirmed" }] });
      expect(await runtime.runPromise(loadOrganizationConnections(organizationId))).toEqual({ kind: "missing" });
      expect(fake.mutations()).toBe(0);
      const saved = await harness.pool.query("select * from organization_pairing where organization_id=$1", [organizationId]);
      expect(saved.rows[0].founder_claim_machine_id).toBe(machineId);
      expect(saved.rows[0].removal_started_at).toBeInstanceOf(Date);
      const retained = saved.rows[0].removal_endpoints[0];
      expect(retained.status).toBe("pending");
      expect(encryption.decrypt(retained.encryptedExpected)).toBe(capability);
      expect(JSON.stringify(saved.rows)).not.toContain(capability);
      expect((await harness.pool.query("select * from organization_machine")).rows).toEqual([]);
      expect(await runtime.runPromise(loadTeardownConnections(organizationId))).toEqual([{ machine_id: machineId, management: capability }]);
    } finally { await runtime.dispose(); }
  });

  it("confirms on the Machine's response and erases credentials", async () => {
    const fake = fixture();
    const runtime = fake.makeRuntime();
    try {
      expect(await runtime.runPromise(revokeOrganizationPairing(organizationId))).toEqual({
        confirmed: true, endpoints: [{ machineId, status: "confirmed" }],
      });
      expect(fake.mutations()).toBe(1);
      expect(fake.dialed.every((options) => options.connections.length === 1)).toBe(true);
      expect((await harness.pool.query("select * from organization_pairing")).rows).toEqual([]);
      expect(await runtime.runPromise(loadOrganizationConnections(organizationId))).toEqual({ kind: "missing" });
      await seedPairing("ppair_new_generation", "ployz1:next-generation");
      const next = await runtime.runPromise(loadOrganizationConnections(organizationId));
      expect(next).toMatchObject({ kind: "ready", connections: [{ management: "ployz1:next-generation" }] });
      expect((await harness.pool.query("select is_dial_entry from organization_machine")).rows).toEqual([{ is_dial_entry: true }]);
    } finally { await runtime.dispose(); }
  });

  it("confirms a lost removal acknowledgement only through an authenticated cleared Management Client response on retry", async () => {
    const fake = fixture();
    fake.endpoint.loseAck = true;
    const runtime = fake.makeRuntime();
    try {
      expect((await runtime.runPromise(revokeOrganizationPairing(organizationId))).confirmed).toBe(false);
      expect(fake.endpoint.paired).toBe(false);
      expect((await harness.pool.query("select removal_endpoints from organization_pairing")).rows[0].removal_endpoints[0].status).toBe("pending");
      expect((await runtime.runPromise(revokeOrganizationPairing(organizationId))).confirmed).toBe(true);
      expect(fake.mutations()).toBe(1);
      expect((await harness.pool.query("select * from organization_pairing")).rows).toEqual([]);
    } finally { await runtime.dispose(); }
  });

  it("does not confirm removal when a replacement key is still active", async () => {
    const fake = fixture();
    fake.endpoint.replaced = true;
    const runtime = fake.makeRuntime();
    try {
      expect(await runtime.runPromise(revokeOrganizationPairing(organizationId))).toMatchObject({ confirmed: false });
      expect(fake.mutations()).toBe(0);
      expect(fake.endpoint.paired).toBe(true);
      expect((await harness.pool.query("select * from organization_pairing")).rowCount).toBe(1);
    } finally { await runtime.dispose(); }
  });

  it("does not treat an unrelated dial failure as confirmation", async () => {
    const fake = fixture();
    fake.endpoint.paired = false;
    fake.endpoint.online = false;
    const runtime = fake.makeRuntime();
    try {
      expect((await runtime.runPromise(revokeOrganizationPairing(organizationId))).confirmed).toBe(false);
      expect(fake.mutations()).toBe(0);
      expect(fake.dialed).toHaveLength(1);
    } finally { await runtime.dispose(); }
  });

  it("reports each retained endpoint and does not replay a mutation against another Machine", async () => {
    await harness.pool.query(`insert into organization_machine
      (organization_id,machine_id,cluster_key,encrypted_capability,is_dial_entry)
      values ($1,$2,$3,$4,false)`, [organizationId, otherMachineId, hashEnrollmentToken(pairing), encryption.encrypt("ployz1:offline-other")]);
    const fake = fixture();
    const runtime = fake.makeRuntime();
    try {
      const outcome = await runtime.runPromise(revokeOrganizationPairing(organizationId));
      expect(outcome.confirmed).toBe(false);
      expect(outcome.endpoints).toEqual(expect.arrayContaining([
        { machineId, status: "confirmed" }, { machineId: otherMachineId, status: "unconfirmed" },
      ]));
      expect(fake.mutations()).toBe(1);
      const saved = await harness.pool.query("select removal_endpoints from organization_pairing");
      expect(saved.rows[0].removal_endpoints.find((entry: { machineId: string }) => entry.machineId === machineId)).toEqual({
        machineId, status: "confirmed",
      });
    } finally { await runtime.dispose(); }
  });

  it("retains an unpublished founding claim as an explicit unconfirmed omission", async () => {
    await harness.pool.query("delete from organization_machine");
    const fake = fixture();
    const runtime = fake.makeRuntime();
    try {
      await runtime.runPromise(disableOrganizationPairing(organizationId));
      expect(await runtime.runPromise(revokeOrganizationPairing(organizationId))).toEqual({
        confirmed: false, endpoints: [{ machineId, status: "unconfirmed" }],
      });
      expect(fake.dialed).toEqual([]);
    } finally { await runtime.dispose(); }
  });
});
