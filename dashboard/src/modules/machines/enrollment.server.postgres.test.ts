import { loadOrganizationConnections } from "#/modules/machines/connections.server";
import type { Client, ConnectOptions, EnrollmentAssignment, EnrollmentSnapshot } from "@ployz/sdk";
import { registerRequestFromEnrollmentIdentity, rustMachineIdSchema } from "./enrollment";
import { ConfigProvider, Effect, Exit, Layer, ManagedRuntime, Result, Schema } from "effect";
import { Inngest } from "inngest";
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
} from "vitest";
import {
  type GithubPostgresTestHarness,
  startGithubPostgresTestHarness,
} from "#/modules/github/github-ingestion.postgres-test-harness";
import {
  completeMachineEnrollment,
  publishMachineEnrollment,
  enrollMachine,
  reserveEnrollmentAssignment,
  hashEnrollmentToken,
  loadOrganizationEnrollmentStatus,
  mintMachineEnrollment,
  resetPendingOrganizationEnrollment,
} from "#/modules/machines/enrollment.server";
import { disableOrganizationPairing, revokeOrganizationPairing } from "#/modules/machines/pairing-removal.server";
import { asTestDouble } from "#/lib/test-double";
import { OrganizationRuntime, OrganizationRuntimeLive } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { InngestClient } from "#/modules/inngest/client";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import {
  makeSecretEncryption,
  SecretEncryption,
} from "#/utils/encrypted-secret.server";

const disposeClients: Array<() => Promise<void>> = [];
const organizationId = "00000000-0000-4000-8000-000000000401";
const userId = "00000000-0000-4000-8000-000000000402";
const tokens = ["pmet_founding_cas_a", "pmet_founding_cas_b"];
const founderMachineId = Schema.decodeUnknownSync(rustMachineIdSchema)("00000000000000000000000000000001");
const tailcat = "tailcat://protected-founder";
const snapshot: EnrollmentSnapshot = { network: "10.42.0.0/16", machines: [], target_versions: {} };
const enrollmentSettings = {
  encryption: makeSecretEncryption("test-app-encryption-secret-1234567890"),
};

function identity(index: number) {
  return {
    protocolVersion: 2 as const,
    machineId: Schema.decodeUnknownSync(rustMachineIdSchema)((index + 1).toString(16).padStart(32, "0")),
    initialPolicy: {
      labels: {},
      accepts_builds: true,
      accepts_services: true,
      accepts_ingress: true,
    },
    name: `node-${index}`,
    publicKey: Buffer.alloc(32, index + 1).toString("base64"),
    advertisedEndpoints: [`10.0.0.${index + 1}:51820`],
    publicIp: `203.0.113.${index + 1}`,
    requestedStorage: "none" as const,
  };
}

function fakeSession(database: GithubPostgresTestHarness["database"]) {
  let registerCalls = 0;
  let observations = 0;
  const fail = { publish: false, conflict: false };
  const published: EnrollmentAssignment[] = [];
  const connected: ConnectOptions[] = [];
  let closed = 0;
  const connection = {
    error: null as Error | null,
    beforeConnect: async (_options: ConnectOptions) => {},
    beforeRegister: async (_assignment: EnrollmentAssignment) => {},
  };
  const coordinator = enrollmentTestClient(database, async (options) => {
    connected.push(options);
    await connection.beforeConnect(options);
    if (connection.error) throw connection.error;
    return asTestDouble<Client>()({
      close: async () => { closed += 1; },
      observeEnrollment: async () => { observations += 1; return snapshot; },
      register: async (assignment: EnrollmentAssignment) => {
        registerCalls += 1;
        published.push(assignment);
        await connection.beforeRegister(assignment);
        if (fail.conflict) throw Object.assign(new Error("Assignment conflicts"), { code: "conflict" });
        if (fail.publish) throw new Error("Lost publication response");
        return { assigned_machine: assignment.machine, visible_peers: [], target_versions: {} };
      },
    });
  });
  return {
    coordinator, connected, connection, published, fail,
    closed: () => closed,
    observations: () => observations,
    registerCalls: () => registerCalls,
  };
}

function enrollmentTestClient(
  database: GithubPostgresTestHarness["database"],
  connect: (options: ConnectOptions) => Promise<Client>,
) {
  const provider = ConfigProvider.fromEnv({
    env: {
      NODE_ENV: "test",
      DATABASE_URL: "postgres://unused",
      APP_URL: "https://cloud.example.test",
      BETTER_AUTH_SECRET: "better-auth-secret",
      GITHUB_CLIENT_ID: "github-client-id",
      GITHUB_CLIENT_SECRET: "github-client-secret",
      APP_ENCRYPTION_SECRET:
        "app-encryption-secret-at-least-32-characters",
    },
  });
  const config = AppConfig.layer.pipe(
    Layer.provide(ConfigProvider.layer(provider)),
  );
  const dependencies = Layer.mergeAll(
    config,
    makePloyzLayer({ connect }),
    Layer.succeed(Database, database),
    Layer.succeed(InngestClient, new Inngest({ id: "enrollment-test" })),
    Layer.succeed(SecretEncryption, enrollmentSettings.encryption),
  );
  const layer = OrganizationRuntimeLive.pipe(Layer.provideMerge(dependencies));
  const runtime = ManagedRuntime.make(layer);
  disposeClients.push(() => runtime.dispose());
  return {
    disable: (id = organizationId) => runtime.runPromise(disableOrganizationPairing(id)),
    publish: (input: Parameters<typeof publishMachineEnrollment>[0]) => runtime.runPromise(
      Effect.result(publishMachineEnrollment(input)),
    ),
    connections: (id = organizationId) => runtime.runPromise(loadOrganizationConnections(id)),
    open: (id = organizationId) => runtime.runPromise(Effect.scoped(
      Effect.gen(function* () { return yield* (yield* OrganizationRuntime).open(id); }),
    )),
    enroll: (input: Parameters<typeof enrollMachine>[0]) =>
      runtime.runPromise(
        Effect.result(enrollMachine(input)),
      ),
    completeFounding: (input: Parameters<typeof completeMachineEnrollment>[0]) =>
      runtime.runPromise(
        Effect.result(
          completeMachineEnrollment(input),
        ),
      ),
    resetPendingEnrollment: (_organizationId: string) =>
      runtime.runPromise(
        Effect.result(
          resetPendingOrganizationEnrollment(
            { userId },
            {
              organizationSlug: "enroll",
              confirmedFounderStoppedOrErased: true,
            },
          ),
        ),
      ),
    tryRevokePairing: (organizationId: string) =>
      runtime.runPromise(
        revokeOrganizationPairing(organizationId).pipe(
          Effect.map((outcome) => outcome.confirmed),
          Effect.provide(layer),
        ),
      ),
  };
}

describe("organization enrollment coordinator", () => {
  let harness: GithubPostgresTestHarness;

  beforeAll(async () => {
    harness = await startGithubPostgresTestHarness();
  }, 60_000);

  afterEach(async () => {
    await Promise.all(disposeClients.splice(0).map((dispose) => dispose()));
  });

  afterAll(async () => {
    await harness.stop();
  });

  beforeEach(async () => {
    await harness.pool.query(`
      truncate table organization_machine, organization_pairing,
        machine_enrollment_token, "user", organization cascade;
      insert into organization (id, name, slug)
      values ('${organizationId}', 'Enroll', 'enroll');
      insert into "user" (id, email, name)
      values ('${userId}', 'enroll@example.com', 'Owner');
      insert into member (id, organization_id, user_id, role, created_at)
      values (gen_random_uuid(), '${organizationId}', '${userId}', 'owner', now());
      insert into machine_enrollment_token (
        organization_id, created_by_user_id, token_hash, expires_at
      ) values
        ('${organizationId}', '${userId}', '${hashEnrollmentToken(tokens[0] ?? "")}', now() + interval '1 day'),
        ('${organizationId}', '${userId}', '${hashEnrollmentToken(tokens[1] ?? "")}', now() + interval '1 day');
    `);
  });

  it("uses Actor and the managed database for enrollment commands", async () => {
    const provider = ConfigProvider.fromEnv({
      env: {
        NODE_ENV: "test",
        DATABASE_URL: harness.databaseUrl,
        APP_URL: "https://cloud.example.test",
        BETTER_AUTH_SECRET: "better-auth-secret",
        GITHUB_CLIENT_ID: "github-client-id",
        GITHUB_CLIENT_SECRET: "github-client-secret",
        APP_ENCRYPTION_SECRET:
          "app-encryption-secret-at-least-32-characters",
      },
    });
    const config = AppConfig.layer.pipe(
      Layer.provide(ConfigProvider.layer(provider)),
    );
    const layer = DatabaseLive.pipe(Layer.provideMerge(config));

    await Effect.runPromise(
      Effect.scoped(
        Effect.gen(function* () {
          const status = yield* loadOrganizationEnrollmentStatus(
            { userId },
            { organizationSlug: "enroll" },
          );
          expect(status).toBe("unclaimed");

          const minted = yield* mintMachineEnrollment(
            { userId },
            { organizationSlug: "enroll" },
          );
          expect(minted.command).toContain("ployz cloud enroll 'pmet_");

          const denied = yield* loadOrganizationEnrollmentStatus(
            { userId: "00000000-0000-4000-8000-000000000499" },
            { organizationSlug: "enroll" },
          ).pipe(Effect.exit);
          expect(Exit.isFailure(denied)).toBe(true);
        }).pipe(Effect.provide(layer)),
      ),
    );

    const tokens = await harness.pool.query<{ count: number }>(
      "select count(*)::int as count from machine_enrollment_token where created_by_user_id = $1",
      [userId],
    );
    expect(tokens.rows[0]?.count).toBe(3);
  });

  it("grants exactly one Organization founding attempt across twenty calls and multiple tokens", async () => {
    const fake = fakeSession(harness.database);
    const coordinator = fake.coordinator;

    const outcomes = await Promise.all(
      Array.from({ length: 20 }, (_, index) =>
        coordinator.enroll({
          token: tokens[index % tokens.length] ?? "",
          identity: identity(index),
        }),
      ),
    );
    const directives = outcomes.flatMap((outcome) =>
      Result.isFailure(outcome) ? [] : [outcome.success],
    );

    expect(directives).toHaveLength(20);
    expect(directives.filter((value) => value.kind === "initialize")).toEqual([
      expect.objectContaining({ kind: "initialize", resumed: false }),
    ]);
    expect(directives.filter((value) => value.kind === "not_yet")).toHaveLength(
      19,
    );
    expect(fake.observations()).toBe(0);
  });

  it("resumes only the matching founder forever and parks every other Machine without connecting", async () => {
    const fake = fakeSession(harness.database);
    fake.connection.error = new Error("Candidate unreachable");
    const coordinator = fake.coordinator;

    const first = await coordinator.enroll({ token: tokens[0] ?? "", identity: identity(0) });
    expect(Result.isFailure(first)).toBe(false);
    if (Result.isFailure(first) || first.success.kind !== "initialize") return;

    await harness.pool.query(`
      update organization_pairing
      set updated_at = now() - interval '1 year'
      where organization_id = '${organizationId}'
    `);
    const resumed = await coordinator.enroll({ token: tokens[1] ?? "", identity: identity(0) });
    const waiter = await coordinator.enroll({ token: tokens[1] ?? "", identity: identity(1) });

    expect(Result.isFailure(resumed)).toBe(false);
    expect(Result.isFailure(waiter)).toBe(false);
    if (Result.isFailure(resumed) || Result.isFailure(waiter)) return;
    expect(resumed.success).toEqual({ ...first.success, resumed: true });
    expect(waiter.success).toEqual({ kind: "not_yet", retryAfter: 2 });
    expect(fake.observations()).toBe(0);
  });

  it("commits ready only after protected publication and scoped negotiation, then joins waiters", async () => {
    const fake = fakeSession(harness.database);
    const coordinator = fake.coordinator;
    const first = await coordinator.enroll({ token: tokens[0] ?? "", identity: identity(0) });
    expect(Result.isFailure(first)).toBe(false);
    if (Result.isFailure(first) || first.success.kind !== "initialize") return;

    const beforePublication = await coordinator.completeFounding({
      token: tokens[1] ?? "",
      machineId: founderMachineId,
      pairingCredential: first.success.pairing.secret,
    });
    expect(Result.isFailure(beforePublication)).toBe(true);

    await coordinator.publish({ token: tokens[0] ?? "", machineId: identity(0).machineId, pairingCredential: first.success.pairing.secret, tailcat });
    const completed = await coordinator.completeFounding({
      token: tokens[1] ?? "",
      machineId: founderMachineId,
      pairingCredential: first.success.pairing.secret,
    });
    const repeated = await coordinator.completeFounding({
      token: tokens[0] ?? "",
      machineId: founderMachineId,
      pairingCredential: first.success.pairing.secret,
    });
    expect(Result.isFailure(completed)).toBe(false);
    expect(Result.isFailure(repeated)).toBe(false);
    if (Result.isFailure(completed) || Result.isFailure(repeated)) return;
    expect(completed.success).toEqual({ machineId: founderMachineId });
    expect(repeated.success).toEqual(completed.success);

    const waiters = await Promise.all(
      Array.from({ length: 19 }, (_, index) =>
        coordinator.enroll({
          token: tokens[index % tokens.length] ?? "",
          identity: identity(index + 1),
        }),
      ),
    );
    expect(
      waiters.every(
        (outcome) => !Result.isFailure(outcome) && outcome.success.kind === "join",
      ),
    ).toBe(true);
    expect(fake.registerCalls()).toBe(19);
    expect(new Set(fake.published.map((a) => a.machine.subnet)).size).toBe(19);
    expect(fake.published.map((a) => a.machine.id).sort()).toEqual(
      Array.from({ length: 19 }, (_, index) => identity(index + 1).machineId).sort(),
    );

    const state = await harness.pool.query<{
      founder_machine_id: string | null;
    }>(
      `select founder_machine_id from organization_pairing
       where organization_id = '${organizationId}'`,
    );
    expect(state.rows[0]?.founder_machine_id).toBe(founderMachineId);

    const calls = fake.registerCalls();
    fake.connection.error = new Error("Candidate unreachable");
    const indeterminate = await coordinator.enroll({
      token: tokens[0] ?? "", identity: identity(31),
    });
    expect(indeterminate).toMatchObject({ failure: { _tag: "PloyzProviderError" } });
    expect(fake.registerCalls()).toBe(calls);
    expect((await harness.pool.query("select founder_machine_id from organization_pairing")).rows)
      .toEqual([{ founder_machine_id: founderMachineId }]);
  });

  it("retains the committed assignment after publication failure and conflicts on changed retry inputs", async () => {
    const fake = fakeSession(harness.database);
    const founder = await fake.coordinator.enroll({ token: (tokens[0] ?? ""), identity: identity(0) });
    if (Result.isFailure(founder) || founder.success.kind !== "initialize") throw new Error("Founder missing");
    await fake.coordinator.publish({ token: tokens[0] ?? "", machineId: identity(0).machineId, pairingCredential: founder.success.pairing.secret, tailcat });
    await fake.coordinator.completeFounding({ token: (tokens[0] ?? ""), machineId: founderMachineId, pairingCredential: founder.success.pairing.secret });
    fake.fail.publish = true;
    const failed = await fake.coordinator.enroll({ token: (tokens[1] ?? ""), identity: identity(1) });
    expect(failed).toMatchObject({ failure: { _tag: "PloyzProviderError" } });
    expect(fake.registerCalls()).toBe(1);
    expect(fake.closed()).toBe(2);
    const saved = await harness.pool.query("select assignments from enrollment_allocation");
    expect(saved.rows[0].assignments).toEqual(fake.published);
    fake.fail.publish = false;
    // A fresh coordinator has no worker-local retry state.
    const fresh = fakeSession(harness.database);
    const resumed = await fresh.coordinator.enroll({ token: (tokens[0] ?? ""), identity: identity(1) });
    expect(resumed).toMatchObject({ success: { kind: "join" } });
    expect(fresh.published).toEqual(fake.published);
    const changed = await fresh.coordinator.enroll({ token: (tokens[0] ?? ""), identity: { ...identity(1), requestedStorage: "zfs" } });
    expect(changed).toMatchObject({ failure: { _tag: "Conflict" } });
    expect(fresh.published).toHaveLength(1);
  });

  it("returns a permanent publication conflict without trying a stale Entry or freeing the assignment", async () => {
    const fake = fakeSession(harness.database);
    const founder = await fake.coordinator.enroll({ token: tokens[0] ?? "", identity: identity(0) });
    if (Result.isFailure(founder) || founder.success.kind !== "initialize") throw new Error("Founder missing");
    await fake.coordinator.publish({ token: tokens[0] ?? "", machineId: identity(0).machineId, pairingCredential: founder.success.pairing.secret, tailcat });
    await fake.coordinator.completeFounding({ token: tokens[0] ?? "", machineId: founderMachineId, pairingCredential: founder.success.pairing.secret });
    fake.fail.conflict = true;

    const outcome = await fake.coordinator.enroll({ token: tokens[1] ?? "", identity: identity(1) });

    expect(outcome).toMatchObject({ failure: { _tag: "Conflict" } });
    expect(fake.registerCalls()).toBe(1);
    const saved = await harness.pool.query("select assignments from enrollment_allocation");
    expect(saved.rows[0].assignments).toEqual(fake.published);
    const retry = await fake.coordinator.enroll({ token: tokens[1] ?? "", identity: identity(1) });
    expect(retry).toMatchObject({ failure: { _tag: "Conflict" } });
    expect(fake.registerCalls()).toBe(2);
    expect(fake.published[1]).toEqual(fake.published[0]);
  });

  it("serializes identical requests and keeps organization and Cluster histories independent", async () => {
    const reserve = (organizationId: string, pairing: string, index: number) => harness.runEffect(
      reserveEnrollmentAssignment({ organizationId, pairing, identity: registerRequestFromEnrollmentIdentity(identity(index)), snapshot }),
    );
    const identical = await Promise.all(Array.from({ length: 20 }, () => reserve(organizationId, "cluster-a", 1)));
    expect(identical.every((assignment) => JSON.stringify(assignment) === JSON.stringify(identical[0]))).toBe(true);
    const next = await reserve(organizationId, "cluster-a", 2);
    expect(next.machine.subnet).not.toBe(identical[0]?.machine.subnet);
    const otherOrganization = "00000000-0000-4000-8000-000000000403";
    await harness.pool.query("insert into organization (id,name,slug) values ($1,'Other','other')", [otherOrganization]);
    const [otherCluster, otherOrg] = await Promise.all([
      reserve(organizationId, "cluster-b", 1), reserve(otherOrganization, "cluster-a", 1),
    ]);
    expect(otherCluster.machine.subnet).toBe(identical[0]?.machine.subnet);
    expect(otherOrg.machine.subnet).toBe(identical[0]?.machine.subnet);
    const histories = await harness.pool.query("select jsonb_array_length(assignments) as count from enrollment_allocation order by count");
    expect(histories.rows).toEqual([{ count: 1 }, { count: 1 }, { count: 2 }]);
  });

  it("releases transaction locks before publication and never retries a failed mutation", async () => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    await fake.coordinator.publish({ ...attempt, tailcat });
    await fake.coordinator.completeFounding(attempt);
    await harness.runEffect(reserveEnrollmentAssignment({
      organizationId, pairing: attempt.pairingCredential,
      identity: registerRequestFromEnrollmentIdentity(identity(3)), snapshot,
    }));
    await fake.coordinator.publish({ ...attempt, machineId: identity(3).machineId, tailcat: "tailcat://other-entry" });
    fake.connection.beforeRegister = async (assignment) => {
      // Independent transaction must proceed while the network call is in flight.
      const next = await harness.runEffect(reserveEnrollmentAssignment({
        organizationId, pairing: attempt.pairingCredential,
        identity: registerRequestFromEnrollmentIdentity(identity(2)), snapshot,
      }));
      expect(next.machine.subnet).not.toBe(assignment.machine.subnet);
    };
    fake.fail.publish = true;
    expect(await fake.coordinator.enroll({ token: attempt.token, identity: identity(1) }))
      .toMatchObject({ failure: { _tag: "PloyzProviderError" } });
    expect(fake.connected).toHaveLength(2);
    expect(fake.connected.at(-1)).toMatchObject({ connections: [
      { tailcat, machine_id: attempt.machineId },
      { tailcat: "tailcat://other-entry", machine_id: identity(3).machineId },
    ] });
    expect(fake.observations()).toBe(1);
    expect(fake.registerCalls()).toBe(1);
    expect(fake.closed()).toBe(2);
  });

  async function pendingFounder(fake: ReturnType<typeof fakeSession>) {
    const result = await fake.coordinator.enroll({ token: tokens[0] ?? "", identity: identity(0) });
    if (Result.isFailure(result) || result.success.kind !== "initialize") throw new Error("Founder missing");
    return { token: tokens[0] ?? "", machineId: identity(0).machineId, pairingCredential: result.success.pairing.secret };
  }

  it("encrypts the authenticated candidate before completion and preserves it on exact retry", async () => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ failure: { _tag: "Conflict" } });
    expect(await fake.coordinator.completeFounding({ ...attempt, stage: "publish", tailcat })).toMatchObject({ success: { machineId: attempt.machineId } });
    const saved = await harness.pool.query("select encrypted_tailcat, cluster_key from organization_machine");
    expect(JSON.stringify(saved.rows)).not.toContain(tailcat);
    expect(enrollmentSettings.encryption.decrypt(saved.rows[0].encrypted_tailcat)).toBe(tailcat);
    expect(saved.rows[0].cluster_key).toBe(hashEnrollmentToken(attempt.pairingCredential));
    expect(await fake.coordinator.publish({ ...attempt, tailcat })).toMatchObject({ success: { machineId: attempt.machineId } });
    expect((await harness.pool.query("select encrypted_tailcat, cluster_key from organization_machine")).rows).toEqual(saved.rows);
    expect(await fake.coordinator.publish({ ...attempt, tailcat: "tailcat://replacement" })).toMatchObject({ failure: { _tag: "Conflict" } });
    expect((await harness.pool.query("select founder_machine_id, founder_claim_machine_id from organization_pairing")).rows).toEqual([
      { founder_machine_id: null, founder_claim_machine_id: attempt.machineId },
    ]);
    expect(fake.connected).toEqual([]);
    expect(fake.observations()).toBe(0);
    expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ success: { machineId: attempt.machineId } });
    expect(fake.connected).toEqual([expect.objectContaining({ connections: [{ tailcat, machine_id: attempt.machineId }] })]);
    expect(fake.closed()).toBe(1);
    expect(fake.observations()).toBe(0);
    expect(await fake.coordinator.enroll({ token: tokens[1] ?? "", identity: identity(0) })).toMatchObject({ success: { kind: "initialize", resumed: true } });
    expect(await fake.coordinator.tryRevokePairing(organizationId)).toBe(false);
    expect(await fake.coordinator.resetPendingEnrollment(organizationId)).toMatchObject({ failure: { _tag: "Conflict" } });
  });

  it("publishes and confirms a joining candidate from its saved assignment without changing the founder", async () => {
    const fake = fakeSession(harness.database);
    const founder = await pendingFounder(fake);
    await fake.coordinator.publish({ ...founder, tailcat });
    await fake.coordinator.completeFounding(founder);
    const joined = await fake.coordinator.enroll({ token: tokens[1] ?? "", identity: identity(1) });
    expect(joined).toMatchObject({ success: { kind: "join" } });
    const assignment = fake.published[0];
    expect(assignment?.machine.id).toBe(identity(1).machineId);
    expect(assignment?.machine.public_key).toEqual(registerRequestFromEnrollmentIdentity(identity(1)).public_key);
    expect(await fake.coordinator.enroll({ token: founder.token, identity: { ...identity(1), publicKey: identity(2).publicKey } }))
      .toMatchObject({ failure: { _tag: "Conflict" } });
    expect(fake.published).toHaveLength(1);

    const candidate = { token: tokens[1] ?? "", machineId: identity(1).machineId, pairingCredential: founder.pairingCredential };
    const connectionCount = fake.connected.length;
    for (const invalid of [
      { ...candidate, machineId: identity(2).machineId },
      { ...candidate, pairingCredential: "ppair_stale" },
      { ...candidate, token: "pmet_invalid" },
    ]) {
      expect(Result.isFailure(await fake.coordinator.publish({ ...invalid, tailcat: "tailcat://joining" }))).toBe(true);
      expect(Result.isFailure(await fake.coordinator.completeFounding(invalid))).toBe(true);
    }
    expect(await fake.coordinator.completeFounding(candidate)).toMatchObject({ failure: { _tag: "Conflict" } });
    expect(fake.connected).toHaveLength(connectionCount);
    expect(await fake.coordinator.completeFounding({ ...candidate, stage: "publish", tailcat: "tailcat://joining" }))
      .toMatchObject({ success: { machineId: candidate.machineId } });
    expect(await fake.coordinator.publish({ ...candidate, tailcat: "tailcat://joining" }))
      .toMatchObject({ success: { machineId: candidate.machineId } });
    expect(await fake.coordinator.publish({ ...candidate, tailcat: "tailcat://replacement" }))
      .toMatchObject({ failure: { _tag: "Conflict" } });

    fake.connection.error = new Error("Joining candidate unavailable");
    expect(await fake.coordinator.completeFounding(candidate)).toMatchObject({ failure: { _tag: "PloyzProviderError" } });
    fake.connection.error = null;
    expect(await fake.coordinator.completeFounding(candidate)).toMatchObject({ success: { machineId: candidate.machineId } });
    expect(await fake.coordinator.completeFounding(candidate)).toMatchObject({ success: { machineId: candidate.machineId } });
    expect(fake.connected.at(-1)).toMatchObject({ connections: [{ tailcat: "tailcat://joining", machine_id: candidate.machineId }] });
    expect((await harness.pool.query("select founder_machine_id, founder_claim_machine_id from organization_pairing")).rows)
      .toEqual([{ founder_machine_id: founder.machineId, founder_claim_machine_id: founder.machineId }]);
    await harness.pool.query("update organization_machine set is_dial_entry = (machine_id = $1)", [candidate.machineId]);
    expect(await fake.coordinator.connections()).toEqual({ kind: "ready", generation: hashEnrollmentToken(founder.pairingCredential), connections: [
      { tailcat: "tailcat://joining", machine_id: candidate.machineId },
      { tailcat, machine_id: founder.machineId },
    ] });
    expect(await fake.coordinator.open()).toMatchObject({ status: "connected" });
    expect(fake.connected.at(-1)).toMatchObject({ connections: [
      { tailcat: "tailcat://joining", machine_id: candidate.machineId },
      { tailcat, machine_id: founder.machineId },
    ] });
  });

  it("rejects unauthorized, stale, and wrong-Machine publications and completions before connecting", async () => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    for (const input of [
      { ...attempt, token: "pmet_invalid" },
      { ...attempt, pairingCredential: "ppair_stale" },
      { ...attempt, machineId: identity(1).machineId },
    ]) {
      expect(Result.isFailure(await fake.coordinator.publish({ ...input, tailcat }))).toBe(true);
      expect(Result.isFailure(await fake.coordinator.completeFounding(input))).toBe(true);
    }
    expect((await harness.pool.query("select * from organization_machine")).rowCount).toBe(0);
    expect(fake.connected).toEqual([]);
    const changedMachine = await fake.coordinator.enroll({ token: attempt.token, identity: { ...identity(0), machineId: identity(1).machineId } });
    expect(changedMachine).toMatchObject({ success: { kind: "not_yet" } });
    await harness.pool.query("update machine_enrollment_token set expires_at = now() - interval '1 second'");
    expect(Result.isFailure(await fake.coordinator.publish({ ...attempt, tailcat }))).toBe(true);
    expect(Result.isFailure(await fake.coordinator.completeFounding(attempt))).toBe(true);
  });

  it("scopes colliding Machine IDs by Organization and current pairing", async () => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    const otherId = "00000000-0000-4000-8000-000000000403";
    const otherToken = "pmet_other_organization";
    await harness.pool.query("insert into organization(id,name,slug) values($1,'Other','other')", [otherId]);
    await harness.pool.query("insert into machine_enrollment_token(organization_id,created_by_user_id,token_hash,expires_at) values($1,$2,$3,now()+interval '1 day')", [otherId, userId, hashEnrollmentToken(otherToken)]);
    const other = await fake.coordinator.enroll({ token: otherToken, identity: identity(0) });
    if (Result.isFailure(other) || other.success.kind !== "initialize") throw new Error("Other founder missing");
    const otherAttempt = { ...attempt, token: otherToken, pairingCredential: other.success.pairing.secret };
    expect(await fake.coordinator.publish({ ...attempt, token: otherToken, tailcat })).toMatchObject({ failure: { _tag: "Conflict" } });
    await fake.coordinator.publish({ ...attempt, tailcat });
    await fake.coordinator.publish({ ...otherAttempt, tailcat: "tailcat://other" });
    expect(await fake.coordinator.connections()).toEqual({ kind: "ready", generation: hashEnrollmentToken(attempt.pairingCredential), connections: [{ tailcat, machine_id: attempt.machineId }] });
    expect(await fake.coordinator.connections(otherId)).toEqual({ kind: "ready", generation: hashEnrollmentToken(otherAttempt.pairingCredential), connections: [{ tailcat: "tailcat://other", machine_id: attempt.machineId }] });
    await harness.pool.query("update organization_pairing set encrypted_pairing_secret = $1::jsonb where organization_id = $2", [JSON.stringify(enrollmentSettings.encryption.encrypt("ppair_replacement")), organizationId]);
    expect(await fake.coordinator.connections()).toEqual({ kind: "ready", generation: hashEnrollmentToken("ppair_replacement"), connections: [] });
    expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ failure: { _tag: "Conflict" } });
    expect(await fake.coordinator.publish({ ...attempt, tailcat })).toMatchObject({ failure: { _tag: "Conflict" } });
    expect(await fake.coordinator.completeFounding({ ...attempt, pairingCredential: "ppair_replacement" })).toMatchObject({ failure: { _tag: "Conflict" } });
    expect(fake.connected).toEqual([]);
  });

  it.each(["connection timed out", "Machine identity mismatch"])("keeps the published claim pending after %s", async (message) => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    await fake.coordinator.publish({ ...attempt, tailcat });
    fake.connection.error = new Error(message);
    expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ failure: { _tag: "PloyzProviderError" } });
    expect((await harness.pool.query("select founder_machine_id from organization_pairing")).rows).toEqual([{ founder_machine_id: null }]);
    expect(await fake.coordinator.enroll({ token: tokens[1] ?? "", identity: identity(0) })).toMatchObject({ success: { kind: "initialize", resumed: true } });
    expect(await fake.coordinator.enroll({ token: tokens[1] ?? "", identity: identity(1) })).toMatchObject({ success: { kind: "not_yet" } });
    fake.connection.error = null;
    expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ success: { machineId: attempt.machineId } });
    expect(fake.observations()).toBe(0);
  });

  it("accepts both exact completions racing the ready transaction", async () => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    await fake.coordinator.publish({ ...attempt, tailcat });
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    fake.connection.beforeConnect = async () => {
      if (fake.connected.length === 2) release();
      await gate;
    };
    const results = await Promise.all([fake.coordinator.completeFounding(attempt), fake.coordinator.completeFounding(attempt)]);
    expect(results.every(Result.isSuccess)).toBe(true);
    expect(fake.closed()).toBe(2);
    expect((await harness.pool.query("select founder_machine_id from organization_pairing")).rows).toEqual([{ founder_machine_id: attempt.machineId }]);
  });

  it("rejects a pairing replacement during joining before allocating or registering", async () => {
    const fake = fakeSession(harness.database);
    const founder = await pendingFounder(fake);
    await fake.coordinator.publish({ ...founder, tailcat });
    await fake.coordinator.completeFounding(founder);
    fake.connection.beforeConnect = async () => {
      await harness.pool.query("update organization_pairing set encrypted_pairing_secret = $1::jsonb where organization_id = $2", [
        JSON.stringify(enrollmentSettings.encryption.encrypt("ppair_replacement")), organizationId,
      ]);
    };
    expect(await fake.coordinator.enroll({ token: founder.token, identity: identity(1) }))
      .toMatchObject({ failure: { _tag: "Conflict" } });
    expect(fake.observations()).toBe(1);
    expect(fake.registerCalls()).toBe(0);
    expect(fake.closed()).toBe(2);
    expect((await harness.pool.query("select * from enrollment_allocation")).rowCount).toBe(0);
    expect((await harness.pool.query("select founder_machine_id from organization_pairing")).rows)
      .toEqual([{ founder_machine_id: founder.machineId }]);
  });

  it("rejects a pairing replacement that races shared connection confirmation", async () => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    await fake.coordinator.publish({ ...attempt, tailcat });
    fake.connection.beforeConnect = async () => {
      await harness.pool.query("update organization_pairing set encrypted_pairing_secret = $1::jsonb where organization_id = $2", [
        JSON.stringify(enrollmentSettings.encryption.encrypt("ppair_replacement")), organizationId,
      ]);
    };
    expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ failure: { _tag: "Conflict" } });
    expect(fake.closed()).toBe(1);
    expect((await harness.pool.query("select founder_machine_id from organization_pairing")).rows).toEqual([{ founder_machine_id: null }]);
    await harness.pool.query("delete from organization_pairing where organization_id = $1", [organizationId]);
    expect(await fake.coordinator.connections()).toEqual({ kind: "missing" });
    expect(await fake.coordinator.tryRevokePairing(organizationId)).toBe(false);
  });

  it("retains an unconfirmed founding claim while reset disables Cloud access", async () => {
    const fake = fakeSession(harness.database);
    expect(await fake.coordinator.resetPendingEnrollment(organizationId)).toMatchObject({ success: { reset: true } });
    expect(await fake.coordinator.tryRevokePairing(organizationId)).toBe(true);
    const attempt = await pendingFounder(fake);
    for (const failed of [false, true]) {
      fake.connection.error = failed ? new Error("Candidate unreachable") : null;
      expect(await fake.coordinator.resetPendingEnrollment(organizationId)).toMatchObject({ failure: { _tag: "Conflict" } });
    }
    expect(fake.observations()).toBe(0);
    expect((await harness.pool.query("select founder_claim_machine_id from organization_pairing")).rows).toEqual([{ founder_claim_machine_id: attempt.machineId }]);
    expect((await harness.pool.query("select * from machine_enrollment_token")).rowCount).toBe(0);
    expect(await fake.coordinator.connections()).toEqual({ kind: "missing" });
    expect(await fake.coordinator.enroll({ token: attempt.token, identity: identity(0) }))
      .toMatchObject({ failure: { _tag: "Unauthorized" } });
    expect(await fake.coordinator.publish({ ...attempt, tailcat }))
      .toMatchObject({ failure: { _tag: "Unauthorized" } });
    const removal = (await harness.pool.query("select removal_started_at, removal_endpoints from organization_pairing")).rows[0];
    expect(removal.removal_started_at).toBeInstanceOf(Date);
    expect(removal.removal_endpoints).toEqual([
      { machineId: attempt.machineId, status: "unknown" },
    ]);
  });

  it.each(["join", "completion"])("removal cancels an in-progress %s connection and retains its protected credential", async (phase) => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    await fake.coordinator.publish({ ...attempt, tailcat });
    if (phase === "join") await fake.coordinator.completeFounding(attempt);
    let entered = () => {};
    const dialing = new Promise<void>((resolve) => { entered = resolve; });
    let aborted = false;
    fake.connection.beforeConnect = (options) => new Promise<void>((_resolve, reject) => {
      if (!("connections" in options) || !options.signal) throw new Error("Expected scoped SDK signal");
      options.signal.addEventListener("abort", () => {
        aborted = true;
        reject(new Error("Cloud access removed"));
      }, { once: true });
      entered();
    });
    const pending = phase === "join"
      ? fake.coordinator.enroll({ token: attempt.token, identity: identity(1) })
      : fake.coordinator.completeFounding(attempt);
    await dialing;
    await fake.coordinator.disable();
    expect(await pending).toMatchObject({ failure: { _tag: "PloyzProviderError" } });
    expect(aborted).toBe(true);
    expect(fake.registerCalls()).toBe(0);
    expect(fake.observations()).toBe(0);
    expect(await fake.coordinator.open()).toEqual({ status: "no_connection" });
    expect((await harness.pool.query("select * from organization_machine")).rowCount).toBe(0);
    expect((await harness.pool.query("select * from enrollment_allocation")).rowCount).toBe(0);
    const row = (await harness.pool.query("select founder_claim_machine_id, founder_machine_id, removal_endpoints from organization_pairing")).rows[0];
    expect(row.founder_claim_machine_id).toBe(attempt.machineId);
    expect(row.founder_machine_id).toBe(phase === "join" ? attempt.machineId : null);
    expect(enrollmentSettings.encryption.decrypt(row.removal_endpoints[0].encryptedExpected)).toBe(tailcat);
  });

  it("opens the actual Organization runtime from one protected candidate without connecting List", async () => {
    const fake = fakeSession(harness.database);
    expect(await fake.coordinator.open()).toEqual({ status: "no_connection" });
    const attempt = await pendingFounder(fake);
    expect(await fake.coordinator.open()).toEqual({ status: "unreachable", error: null });
    await fake.coordinator.publish({ ...attempt, tailcat });
    expect(await fake.coordinator.open()).toMatchObject({ status: "connected" });
    expect(fake.connected).toEqual([expect.objectContaining({ connections: [{ tailcat, machine_id: attempt.machineId }] })]);
    expect(fake.closed()).toBe(1);
    fake.connection.error = new Error("candidate unreachable");
    expect(await fake.coordinator.open()).toMatchObject({ status: "unreachable", error: { _tag: "PloyzProviderError" } });
    expect(fake.observations()).toBe(0);
    expect((await harness.pool.query("select founder_machine_id from organization_pairing")).rows).toEqual([{ founder_machine_id: null }]);
  });

  it("keeps pairing decrypt failures in the typed Effect channel", async () => {
    const stale = makeSecretEncryption("stale-app-encryption-secret-1234567890");
    await harness.pool.query(
      `insert into organization_pairing (
        organization_id, encrypted_pairing_secret, founder_public_key, founder_claim_machine_id
      ) values ($1, $2::jsonb, $3, $4)`,
      [
        organizationId,
        JSON.stringify(stale.encrypt("ppair_stale")),
        identity(0).publicKey,
        identity(0).machineId,
      ],
    );

    const fake = fakeSession(harness.database);
    const enrolled = await fake.coordinator.enroll({
      token: tokens[0] ?? "",
      identity: identity(0),
    });
    expect(Result.isFailure(enrolled)).toBe(true);
    if (!Result.isFailure(enrolled)) return;
    expect(enrolled.failure).toMatchObject({ _tag: "PairingSecretDecryptFailure" });

    await expect(fake.coordinator.tryRevokePairing(organizationId)).rejects.toMatchObject({ _tag: "Conflict" });
    expect((await harness.pool.query("select removal_started_at from organization_pairing")).rows)
      .toEqual([{ removal_started_at: null }]);
  });

  it("requires a valid Machine owner for pending and ready claims", async () => {
    for (const completedMachineId of [null, identity(0).machineId]) {
      await expect(harness.pool.query(`
        insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_public_key, founder_machine_id)
        values ($1, '{}'::jsonb, 'founder', $2)
      `, [organizationId, completedMachineId])).rejects.toMatchObject({ code: "23502" });
      await expect(harness.pool.query(`
        insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_public_key, founder_machine_id, founder_claim_machine_id)
        values ($1, '{}'::jsonb, 'founder', $2, 'not-a-machine')
      `, [organizationId, completedMachineId])).rejects.toMatchObject({ code: "23514" });
    }
  });


  it("rejects invalid pending and ready Organization Pairing shapes", async () => {
    await expect(
      harness.pool.query(`
        insert into organization_pairing (
          organization_id, encrypted_pairing_secret, founder_claim_machine_id
        ) values ('${organizationId}', '{}'::jsonb, '${identity(0).machineId}')
      `),
    ).rejects.toMatchObject({ code: "23514" });

    await expect(
      harness.pool.query(`
        insert into organization_pairing (
          organization_id, encrypted_pairing_secret, founder_machine_id, founder_claim_machine_id
        ) values (
          '${organizationId}', '{}'::jsonb,
          'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
        )
      `),
    ).resolves.toBeDefined();

    await harness.pool.query(
      `delete from organization_pairing where organization_id = '${organizationId}'`,
    );

    await expect(
      harness.pool.query(`
        insert into organization_pairing (
          organization_id, encrypted_pairing_secret,
          founder_public_key, founder_machine_id, founder_claim_machine_id
        ) values (
          '${organizationId}', '{}'::jsonb, 'founder', 'not-a-machine', '${identity(0).machineId}'
        )
      `),
    ).rejects.toMatchObject({ code: "23514" });
  });
});
