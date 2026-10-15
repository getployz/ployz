import { testConfigEnvironment } from "#/test/config-environment";
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
  vi,
} from "vitest";
import {
  type PostgresTestHarness,
  startPostgresTestHarness,
} from "#/test/postgres";
import {
  completeMachineEnrollment,
  publishMachineEnrollment,
  enrollMachine,
  reserveEnrollmentAssignment,
  hashEnrollmentToken,
  mintCliMachineEnrollment,
  CLUSTER_UNREACHABLE,
  mintMachineEnrollment,
  readMachineEnrollment,
  recordMachineSetupReport,
  resetPendingOrganizationEnrollment,
} from "#/modules/machines/enrollment.server";
import { PostHog, type PostHogService } from "#/modules/analytics/posthog.server";
import type { SetupReport } from "#/modules/machines/enrollment";
import { disableOrganizationPairing, dropRemovedServer, revokeOrganizationPairing } from "#/modules/machines/pairing-removal.server";
import { asTestDouble } from "#/lib/test-double";
import { readCollection } from "#/collections/read.server";
import { OrganizationRuntime, OrganizationRuntimeLive } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { InngestClient } from "#/modules/inngest/client";
import { GithubApi } from "#/modules/github/github-observation.api";
import { Polar } from "#/modules/billing/polar-provider.server";
import { fakeGithubApi } from "#/test/fake-github";
import { CloudStoreLive } from "#/modules/config-store/store-sdk.server";
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
const capability = "ployz1:protected-founder";
const snapshot: EnrollmentSnapshot = { network: "10.42.0.0/16", machines: [], target_versions: {} };
const enrollmentSettings = {
  encryption: makeSecretEncryption("test-app-encryption-secret-1234567890"),
};

function identity(index: number) {
  return {
    protocolVersion: 1 as const,
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

function fakeSession(database: PostgresTestHarness["database"]) {
  let registerCalls = 0;
  let observations = 0;
  const fail = { publish: false };
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
  database: PostgresTestHarness["database"],
  connect: (options: ConnectOptions) => Promise<Client>,
) {
  const provider = ConfigProvider.fromEnv({
    env: {
      ...testConfigEnvironment(),
      NODE_ENV: "test",
      DATABASE_URL: "postgres://unused",
      APP_URL: "https://cloud.example.test",
    },
  });
  const config = AppConfig.layer.pipe(
    Layer.provide(ConfigProvider.layer(provider)),
  );
  // Completion requests a Cluster Domain sync; the test never reaches Inngest.
  const inngest = new Inngest({ id: "enrollment-test" });
  vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
  const dependencies = Layer.mergeAll(
    config,
    makePloyzLayer({ connect }),
    Layer.succeed(Database, database),
    Layer.succeed(InngestClient, inngest),
    Layer.succeed(SecretEncryption, enrollmentSettings.encryption),
    // The founder's join asks for its published Environments to deploy (an Inngest event); these tests publish none.
    Layer.succeed(GithubApi, fakeGithubApi().service),
    Layer.succeed(Polar, { mode: "self_hosted" }),
  );
  const layer = Layer.mergeAll(OrganizationRuntimeLive, CloudStoreLive).pipe(Layer.provideMerge(dependencies));
  const runtime = ManagedRuntime.make(layer);
  disposeClients.push(() => runtime.dispose());
  return {
    inngest,
    disable: (id = organizationId) => runtime.runPromise(disableOrganizationPairing(id)),
    dropRemoved: (input: Parameters<typeof dropRemovedServer>[0]) => runtime.runPromise(dropRemovedServer(input)),
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
    mintCli: () => runtime.runPromise(mintCliMachineEnrollment({ userId }, { organizationSlug: "enroll" })),
    read: (id: string, actor = userId) => runtime.runPromise(
      Effect.result(readMachineEnrollment({ userId: actor }, { organizationSlug: "enroll", id })),
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
  let harness: PostgresTestHarness;

  beforeAll(async () => {
    harness = await startPostgresTestHarness();
  }, 60_000);

  afterEach(async () => {
    await Promise.all(disposeClients.splice(0).map((dispose) => dispose()));
  });

  afterAll(async () => {
    await harness.stop();
  });

  let issued = 0;
  /** An enrollment token nobody has claimed yet, or one already claimed by `claimedBy`. */
  async function issueToken(claimedBy?: ReturnType<typeof identity>) {
    issued += 1;
    const token = `pmet_issued_${issued}`;
    await harness.pool.query(`insert into machine_enrollment_token (
      organization_id, created_by_user_id, token_hash, expires_at, claimed_machine_id, claimed_public_key, claimed_at
    ) values ($1, $2, $3, now() + interval '1 day', $4, $5, case when $4::text is null then null else now() end)`, [
      organizationId, userId, hashEnrollmentToken(token), claimedBy?.machineId ?? null, claimedBy?.publicKey ?? null,
    ]);
    return token;
  }

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
        ...testConfigEnvironment(),
        NODE_ENV: "test",
        DATABASE_URL: harness.databaseUrl,
        APP_URL: "https://cloud.example.test",
      },
    });
    const config = AppConfig.layer.pipe(
      Layer.provide(ConfigProvider.layer(provider)),
    );
    const layer = DatabaseLive.pipe(Layer.provideMerge(config));

    await Effect.runPromise(
      Effect.scoped(
        Effect.gen(function* () {
          const enrollment = { table: "organization_enrollment", organizationSlug: "enroll", userId } as const;
          // No pairing row: the Org Store holds no enrollment row, which reads as unclaimed.
          expect((yield* readCollection({ userId }, enrollment)).rows).toEqual([]);

          const minted = yield* mintMachineEnrollment(
            { userId },
            { organizationSlug: "enroll" },
          );
          expect(minted.command).toContain("ployz server add --token 'pmet_");

          const outsider = "00000000-0000-4000-8000-000000000499";
          const denied = yield* readCollection({ userId: outsider }, { ...enrollment, userId: outsider }).pipe(Effect.exit);
          expect(Exit.isFailure(denied)).toBe(true);

          yield* Effect.promise(() => harness.pool.query(`insert into organization_pairing
            (organization_id, encrypted_pairing_secret, founder_public_key, founder_claim_machine_id)
            values ($1, '{"secret":"never-leaves-the-server"}'::jsonb, 'founder', $2)`, [organizationId, identity(0).machineId]));
          // Only the derived status leaves the server; the encrypted pairing secret never does.
          expect((yield* readCollection({ userId }, enrollment)).rows).toEqual([{ id: organizationId, status: "pending" }]);
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
    const resumed = await coordinator.enroll({ token: tokens[0] ?? "", identity: identity(0) });
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
      token: tokens[0] ?? "",
      machineId: founderMachineId,
      pairingCredential: first.success.pairing.secret,
    });
    expect(Result.isFailure(beforePublication)).toBe(true);

    await coordinator.publish({ token: tokens[0] ?? "", machineId: identity(0).machineId, pairingCredential: first.success.pairing.secret, capability });
    const completed = await coordinator.completeFounding({
      token: tokens[0] ?? "",
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
    // Only a deployment that needs a generated hostname reserves the Cluster Domain.
    expect((await harness.pool.query("select name from organization_cluster_domain")).rows).toEqual([]);

    const waiterTokens = await Promise.all(Array.from({ length: 19 }, () => issueToken()));
    const waiters = await Promise.all(
      waiterTokens.map((token, index) => coordinator.enroll({ token, identity: identity(index + 1) })),
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
      token: await issueToken(), identity: identity(31),
    });
    expect(indeterminate).toMatchObject({ failure: { _tag: "Conflict", message: CLUSTER_UNREACHABLE } });
    expect(fake.registerCalls()).toBe(calls);
    expect((await harness.pool.query("select founder_machine_id from organization_pairing")).rows)
      .toEqual([{ founder_machine_id: founderMachineId }]);
  });

  it("retains the committed assignment after publication failure and conflicts on changed retry inputs", async () => {
    const fake = fakeSession(harness.database);
    const founder = await fake.coordinator.enroll({ token: (tokens[0] ?? ""), identity: identity(0) });
    if (Result.isFailure(founder) || founder.success.kind !== "initialize") throw new Error("Founder missing");
    await fake.coordinator.publish({ token: tokens[0] ?? "", machineId: identity(0).machineId, pairingCredential: founder.success.pairing.secret, capability });
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
    const resumed = await fresh.coordinator.enroll({ token: (tokens[1] ?? ""), identity: identity(1) });
    expect(resumed).toMatchObject({ success: { kind: "join" } });
    expect(fresh.published).toEqual(fake.published);
    const changed = await fresh.coordinator.enroll({ token: (tokens[1] ?? ""), identity: { ...identity(1), requestedStorage: "zfs" } });
    expect(changed).toMatchObject({ failure: { _tag: "Conflict" } });
    expect(fresh.published).toHaveLength(1);
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
    await fake.coordinator.publish({ ...attempt, capability });
    await fake.coordinator.completeFounding(attempt);
    await harness.runEffect(reserveEnrollmentAssignment({
      organizationId, pairing: attempt.pairingCredential,
      identity: registerRequestFromEnrollmentIdentity(identity(3)), snapshot,
    }));
    await fake.coordinator.publish({
      ...attempt, token: await issueToken(identity(3)), machineId: identity(3).machineId, capability: "ployz1:other-entry",
    });
    fake.connection.beforeRegister = async (assignment) => {
      // Independent transaction must proceed while the network call is in flight.
      const next = await harness.runEffect(reserveEnrollmentAssignment({
        organizationId, pairing: attempt.pairingCredential,
        identity: registerRequestFromEnrollmentIdentity(identity(2)), snapshot,
      }));
      expect(next.machine.subnet).not.toBe(assignment.machine.subnet);
    };
    fake.fail.publish = true;
    expect(await fake.coordinator.enroll({ token: await issueToken(), identity: identity(1) }))
      .toMatchObject({ failure: { _tag: "PloyzProviderError" } });
    expect(fake.connected).toHaveLength(2);
    expect(fake.connected.at(-1)).toMatchObject({ connections: [
      { management: capability, machine_id: attempt.machineId },
      { management: "ployz1:other-entry", machine_id: identity(3).machineId },
    ] });
    expect(fake.observations()).toBe(1);
    expect(fake.registerCalls()).toBe(1);
    expect(fake.closed()).toBe(2);
  });

  async function pendingFounder(fake: ReturnType<typeof fakeSession>, token = tokens[0] ?? "") {
    const result = await fake.coordinator.enroll({ token, identity: identity(0) });
    if (Result.isFailure(result) || result.success.kind !== "initialize") throw new Error("Founder missing");
    return { token, machineId: identity(0).machineId, pairingCredential: result.success.pairing.secret };
  }

  async function readyFounder(fake: ReturnType<typeof fakeSession>) {
    const founder = await pendingFounder(fake);
    await fake.coordinator.publish({ ...founder, capability });
    await fake.coordinator.completeFounding(founder);
    return founder;
  }

  it("reports the Server that completed enrollment with a CLI-minted token, and only to members", async () => {
    const fake = fakeSession(harness.database);
    const minted = await fake.coordinator.mintCli();
    expect(minted.token).toMatch(/^pmet_/u);
    expect(await fake.coordinator.read(minted.id)).toMatchObject({ success: { status: "pending" } });
    expect(await fake.coordinator.read(minted.id, "00000000-0000-4000-8000-000000000499")).toMatchObject({ failure: {} });

    const attempt = await pendingFounder(fake, minted.token);
    const founder = attempt;
    await fake.coordinator.publish({ ...attempt, capability });
    expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ success: { machineId: founder.machineId } });
    expect(await fake.coordinator.read(minted.id)).toMatchObject({ success: { status: "joined", machineId: founder.machineId } });

    await harness.pool.query("update machine_enrollment_token set joined_machine_id = null, expires_at = now() - interval '1 second' where id = $1", [minted.id]);
    expect(await fake.coordinator.read(minted.id)).toMatchObject({ success: { status: "expired" } });
    expect(await fake.coordinator.read("00000000-0000-4000-8000-000000000498")).toMatchObject({ failure: { _tag: "NotFound" } });
  });

  it("captures a setup report for the token's creator only when the token fits the outcome", async () => {
    const [pending = "", joined = ""] = tokens;
    await harness.pool.query("update machine_enrollment_token set joined_machine_id = $1 where token_hash = $2", [
      founderMachineId,
      hashEnrollmentToken(joined),
    ]);
    const steps = [{ name: "install", seconds: 1 }] as const;
    const succeeded: SetupReport = { outcome: "succeeded", profile: { arch: "x86_64" }, steps, totalSeconds: 2 };
    const failed: SetupReport = {
      outcome: "failed", profile: { arch: "x86_64" }, steps, totalSeconds: 2,
      failedStep: "enroll", failedStepSeconds: 1, error: "refused",
    };
    const record = async (token: string, report: SetupReport) => {
      const captured: Parameters<PostHogService["capture"]>[0][] = [];
      const recorder: PostHogService = {
        capture: (input) => Effect.sync(() => void captured.push(input)),
        identify: () => Effect.void,
        identifyOrganization: () => Effect.void,
        toolCall: () => Effect.void,
      };
      const result = await Effect.runPromise(recordMachineSetupReport({ token, report }).pipe(
        Effect.result,
        Effect.provideService(Database, harness.database),
        Effect.provideService(PostHog, recorder),
      ));
      return { ok: Result.isSuccess(result), captured };
    };

    expect(await record(joined, succeeded)).toEqual({ ok: true, captured: [{
      userId,
      organizationId,
      event: "server_setup_succeeded",
      properties: { arch: "x86_64", step_install_seconds: 1, total_seconds: 2 },
    }] });
    expect(await record(pending, failed)).toEqual({ ok: true, captured: [{
      userId,
      organizationId,
      event: "server_setup_failed",
      properties: {
        arch: "x86_64", step_install_seconds: 1, total_seconds: 2,
        failed_step: "enroll", failed_step_seconds: 1, error: "refused",
      },
    }] });
    for (const [token, report] of [[pending, succeeded], [joined, failed], ["pmet_unknown", succeeded]] as const) {
      expect(await record(token, report), `${token} ${report.outcome}`).toEqual({ ok: false, captured: [] });
    }
  });

  it("encrypts the authenticated candidate before completion and preserves it on exact retry", async () => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ failure: { _tag: "Conflict" } });
    expect(await fake.coordinator.completeFounding({ ...attempt, stage: "publish", capability })).toMatchObject({ success: { machineId: attempt.machineId } });
    const saved = await harness.pool.query("select encrypted_capability, cluster_key from organization_machine");
    expect(JSON.stringify(saved.rows)).not.toContain(capability);
    expect(enrollmentSettings.encryption.decrypt(saved.rows[0].encrypted_capability)).toBe(capability);
    expect(saved.rows[0].cluster_key).toBe(hashEnrollmentToken(attempt.pairingCredential));
    expect(await fake.coordinator.publish({ ...attempt, capability })).toMatchObject({ success: { machineId: attempt.machineId } });
    expect((await harness.pool.query("select encrypted_capability, cluster_key from organization_machine")).rows).toEqual(saved.rows);
    expect((await harness.pool.query("select founder_machine_id, founder_claim_machine_id from organization_pairing")).rows).toEqual([
      { founder_machine_id: null, founder_claim_machine_id: attempt.machineId },
    ]);
    expect(fake.connected).toEqual([]);
    expect(fake.observations()).toBe(0);
    expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ success: { machineId: attempt.machineId } });
    expect(fake.connected).toEqual([expect.objectContaining({ connections: [{ management: capability, machine_id: attempt.machineId }] })]);
    expect(fake.closed()).toBe(1);
    expect(fake.observations()).toBe(0);
    expect(await fake.coordinator.enroll({ token: tokens[1] ?? "", identity: identity(0) })).toMatchObject({ success: { kind: "initialize", resumed: true } });
    expect(await fake.coordinator.tryRevokePairing(organizationId)).toBe(false);
    expect(await fake.coordinator.resetPendingEnrollment(organizationId)).toMatchObject({ failure: { _tag: "Conflict" } });
  });

  it("refreshes credentials after a lost acknowledgement", async () => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    await fake.coordinator.publish({ ...attempt, capability });
    expect(await fake.coordinator.enroll({ token: attempt.token, identity: identity(0) }))
      .toMatchObject({ success: { kind: "initialize", resumed: true } });
    const before = await harness.pool.query("select cluster_key, is_dial_entry from organization_machine");
    expect(await fake.coordinator.publish({ ...attempt, capability: "ployz1:rotated" }))
      .toMatchObject({ success: { machineId: attempt.machineId } });
    expect(fake.connected.at(-1)).toMatchObject({ connections: [{ management: "ployz1:rotated", machine_id: attempt.machineId }] });
    expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ success: { machineId: attempt.machineId } });
    expect(fake.connected.at(-1)).toMatchObject({ connections: [{ management: "ployz1:rotated", machine_id: attempt.machineId }] });
    expect((await harness.pool.query("select cluster_key, is_dial_entry from organization_machine")).rows).toEqual(before.rows);
    expect(fake.closed()).toBe(fake.connected.length);
  });

  const keyed = (key: number, secret = 0xee) => `ployz1:${Buffer.concat([Buffer.alloc(32, key), Buffer.alloc(32, secret)]).toString("base64url")}`;

  it("keeps the management key a refreshed capability carries, and asks for its push target again", async () => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    const saved = async () => (await harness.pool.query("select management_identity, push_target from organization_machine")).rows;
    const requests = () => vi.mocked(fake.coordinator.inngest.send).mock.calls
      .filter(([event]) => !Array.isArray(event) && "name" in event && event.name === "observe/push-target.requested").length;
    await fake.coordinator.publish({ ...attempt, capability: keyed(0xab) });
    expect(await saved()).toEqual([{ management_identity: "ab".repeat(32), push_target: null }]);
    expect(requests()).toBe(1);
    await harness.pool.query("update organization_machine set push_target = 'https://cloud.example.test/api/machines/x/observe'");
    expect(await fake.coordinator.publish({ ...attempt, capability: keyed(0xab, 0x11) }))
      .toMatchObject({ success: { machineId: attempt.machineId } });
    expect(await saved()).toEqual([{ management_identity: "ab".repeat(32), push_target: null }]);
    expect(requests()).toBe(2);
  });

  it("refuses a refresh that hands Cloud a different management key", async () => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    await fake.coordinator.publish({ ...attempt, capability: keyed(0xab) });
    const before = await harness.pool.query("select * from organization_machine");
    const connections = fake.connected.length;
    expect(await fake.coordinator.publish({ ...attempt, capability: keyed(0xcd) })).toMatchObject({
      failure: { _tag: "Conflict", message: "The Machine's management identity changed. Remove the Server and enroll it again.", userFacing: true },
    });
    expect((await harness.pool.query("select * from organization_machine")).rows).toEqual(before.rows);
    expect(enrollmentSettings.encryption.decrypt(before.rows[0].encrypted_capability)).toBe(keyed(0xab));
    expect(fake.connected).toHaveLength(connections);
  });

  it.each(["unreachable", "identity mismatch"])("preserves saved credentials when refresh verification fails: %s", async (message) => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    await fake.coordinator.publish({ ...attempt, capability });
    const before = await harness.pool.query("select * from organization_machine");
    fake.connection.error = new Error(message);
    expect(await fake.coordinator.publish({ ...attempt, capability: "ployz1:unverified" }))
      .toMatchObject({ failure: { _tag: "PloyzProviderError" } });
    expect((await harness.pool.query("select * from organization_machine")).rows).toEqual(before.rows);
  });

  it.each(["new publication", "removal", "expired token"])("rejects a refresh overtaken by %s", async (change) => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    await fake.coordinator.publish({ ...attempt, capability });
    let entered = () => {};
    let release = () => {};
    const verifying = new Promise<void>((resolve) => { entered = resolve; });
    const resume = new Promise<void>((resolve) => { release = resolve; });
    fake.connection.beforeConnect = async (options) => {
      if ("connections" in options && options.connections.some((connection) => "management" in connection && connection.management === "ployz1:delayed")) {
        entered();
        await resume;
      }
    };
    const delayed = fake.coordinator.publish({ ...attempt, capability: "ployz1:delayed" });
    await verifying;
    try {
      if (change === "new publication") {
        expect(await fake.coordinator.publish({ ...attempt, capability: "ployz1:newer" }))
          .toMatchObject({ success: { machineId: attempt.machineId } });
      } else if (change === "removal") {
        await fake.coordinator.disable();
      } else {
        await harness.pool.query("update machine_enrollment_token set expires_at = now() - interval '1 second'");
      }
      const before = await harness.pool.query("select * from organization_machine");
      release();
      expect(Result.isFailure(await delayed)).toBe(true);
      expect((await harness.pool.query("select * from organization_machine")).rows).toEqual(before.rows);
      expect(fake.closed()).toBe(fake.connected.length);
    } finally {
      release();
      await delayed;
    }
  });

  it("publishes and confirms a joining candidate from its saved assignment without changing the founder", async () => {
    const fake = fakeSession(harness.database);
    const founder = await pendingFounder(fake);
    await fake.coordinator.publish({ ...founder, capability });
    await fake.coordinator.completeFounding(founder);
    const joined = await fake.coordinator.enroll({ token: tokens[1] ?? "", identity: identity(1) });
    expect(joined).toMatchObject({ success: { kind: "join" } });
    const assignment = fake.published[0];
    expect(assignment?.machine.id).toBe(identity(1).machineId);
    expect(assignment?.machine.public_key).toEqual(registerRequestFromEnrollmentIdentity(identity(1)).public_key);
    expect(await fake.coordinator.enroll({ token: await issueToken(), identity: { ...identity(1), publicKey: identity(2).publicKey } }))
      .toMatchObject({ failure: { _tag: "Conflict" } });
    expect(fake.published).toHaveLength(1);

    const candidate = { token: tokens[1] ?? "", machineId: identity(1).machineId, pairingCredential: founder.pairingCredential };
    const connectionCount = fake.connected.length;
    for (const invalid of [
      { ...candidate, machineId: identity(2).machineId },
      { ...candidate, pairingCredential: "ppair_stale" },
      { ...candidate, token: "pmet_invalid" },
    ]) {
      expect(Result.isFailure(await fake.coordinator.publish({ ...invalid, capability: "ployz1:joining" }))).toBe(true);
      expect(Result.isFailure(await fake.coordinator.completeFounding(invalid))).toBe(true);
    }
    expect(await fake.coordinator.completeFounding(candidate)).toMatchObject({ failure: { _tag: "Conflict" } });
    expect(fake.connected).toHaveLength(connectionCount);
    expect(await fake.coordinator.completeFounding({ ...candidate, stage: "publish", capability: "ployz1:joining" }))
      .toMatchObject({ success: { machineId: candidate.machineId } });
    expect(await fake.coordinator.publish({ ...candidate, capability: "ployz1:joining" }))
      .toMatchObject({ success: { machineId: candidate.machineId } });
    expect(await fake.coordinator.publish({ ...candidate, capability: "ployz1:replacement" }))
      .toMatchObject({ success: { machineId: candidate.machineId } });

    fake.connection.error = new Error("Joining candidate unavailable");
    expect(await fake.coordinator.completeFounding(candidate)).toMatchObject({ failure: { _tag: "PloyzProviderError" } });
    fake.connection.error = null;
    expect(await fake.coordinator.completeFounding(candidate)).toMatchObject({ success: { machineId: candidate.machineId } });
    expect(await fake.coordinator.completeFounding(candidate)).toMatchObject({ success: { machineId: candidate.machineId } });
    expect(fake.connected.at(-1)).toMatchObject({ connections: [{ management: "ployz1:replacement", machine_id: candidate.machineId }] });
    expect((await harness.pool.query("select founder_machine_id, founder_claim_machine_id from organization_pairing")).rows)
      .toEqual([{ founder_machine_id: founder.machineId, founder_claim_machine_id: founder.machineId }]);
    await harness.pool.query("update organization_machine set is_dial_entry = (machine_id = $1)", [candidate.machineId]);
    expect(await fake.coordinator.connections()).toEqual({ kind: "ready", generation: hashEnrollmentToken(founder.pairingCredential), connections: [
      { management: "ployz1:replacement", machine_id: candidate.machineId },
      { management: capability, machine_id: founder.machineId },
    ] });
    expect(await fake.coordinator.open()).toMatchObject({ status: "connected" });
    expect(fake.connected.at(-1)).toMatchObject({ connections: [
      { management: "ployz1:replacement", machine_id: candidate.machineId },
      { management: capability, machine_id: founder.machineId },
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
      expect(Result.isFailure(await fake.coordinator.publish({ ...input, capability }))).toBe(true);
      expect(Result.isFailure(await fake.coordinator.completeFounding(input))).toBe(true);
    }
    expect((await harness.pool.query("select * from organization_machine")).rowCount).toBe(0);
    expect(fake.connected).toEqual([]);
    const changedMachine = await fake.coordinator.enroll({ token: attempt.token, identity: { ...identity(0), machineId: identity(1).machineId } });
    expect(changedMachine).toMatchObject({ failure: { _tag: "Unauthorized" } });
    await harness.pool.query("update machine_enrollment_token set expires_at = now() - interval '1 second'");
    expect(Result.isFailure(await fake.coordinator.publish({ ...attempt, capability }))).toBe(true);
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
    expect(await fake.coordinator.publish({ ...attempt, token: otherToken, capability })).toMatchObject({ failure: { _tag: "Conflict" } });
    await fake.coordinator.publish({ ...attempt, capability });
    await fake.coordinator.publish({ ...otherAttempt, capability: "ployz1:other" });
    expect(await fake.coordinator.connections()).toEqual({ kind: "ready", generation: hashEnrollmentToken(attempt.pairingCredential), connections: [{ management: capability, machine_id: attempt.machineId }] });
    expect(await fake.coordinator.connections(otherId)).toEqual({ kind: "ready", generation: hashEnrollmentToken(otherAttempt.pairingCredential), connections: [{ management: "ployz1:other", machine_id: attempt.machineId }] });
    await harness.pool.query("update organization_pairing set encrypted_pairing_secret = $1::jsonb where organization_id = $2", [JSON.stringify(enrollmentSettings.encryption.encrypt("ppair_replacement")), organizationId]);
    expect(await fake.coordinator.connections()).toEqual({ kind: "ready", generation: hashEnrollmentToken("ppair_replacement"), connections: [] });
    expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ failure: { _tag: "Conflict" } });
    expect(await fake.coordinator.publish({ ...attempt, capability })).toMatchObject({ failure: { _tag: "Conflict" } });
    expect(await fake.coordinator.completeFounding({ ...attempt, pairingCredential: "ppair_replacement" })).toMatchObject({ failure: { _tag: "Conflict" } });
    expect(fake.connected).toEqual([]);
  });

  it.each(["connection timed out", "Machine identity mismatch"])("keeps the published claim pending after %s", async (message) => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    await fake.coordinator.publish({ ...attempt, capability });
    fake.connection.error = new Error(message);
    expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ failure: { _tag: "PloyzProviderError" } });
    expect((await harness.pool.query("select founder_machine_id from organization_pairing")).rows).toEqual([{ founder_machine_id: null }]);
    expect(await fake.coordinator.enroll({ token: tokens[1] ?? "", identity: identity(0) })).toMatchObject({ success: { kind: "initialize", resumed: true } });
    expect(await fake.coordinator.enroll({ token: await issueToken(), identity: identity(1) })).toMatchObject({ success: { kind: "not_yet" } });
    fake.connection.error = null;
    expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ success: { machineId: attempt.machineId } });
    expect(fake.observations()).toBe(0);
  });

  it("accepts both exact completions racing the ready transaction", async () => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    await fake.coordinator.publish({ ...attempt, capability });
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
    await fake.coordinator.publish({ ...founder, capability });
    await fake.coordinator.completeFounding(founder);
    fake.connection.beforeConnect = async () => {
      await harness.pool.query("update organization_pairing set encrypted_pairing_secret = $1::jsonb where organization_id = $2", [
        JSON.stringify(enrollmentSettings.encryption.encrypt("ppair_replacement")), organizationId,
      ]);
    };
    expect(await fake.coordinator.enroll({ token: await issueToken(), identity: identity(1) }))
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
    await fake.coordinator.publish({ ...attempt, capability });
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
    expect(await fake.coordinator.publish({ ...attempt, capability }))
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
    await fake.coordinator.publish({ ...attempt, capability });
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
      ? fake.coordinator.enroll({ token: await issueToken(), identity: identity(1) })
      : fake.coordinator.completeFounding(attempt);
    await dialing;
    await fake.coordinator.disable();
    expect(await pending).toMatchObject({ failure: { _tag: phase === "join" ? "Conflict" : "PloyzProviderError" } });
    expect(aborted).toBe(true);
    expect(fake.registerCalls()).toBe(0);
    expect(fake.observations()).toBe(0);
    expect(await fake.coordinator.open()).toEqual({ status: "no_connection" });
    expect((await harness.pool.query("select * from organization_machine")).rowCount).toBe(0);
    expect((await harness.pool.query("select * from enrollment_allocation")).rowCount).toBe(0);
    const row = (await harness.pool.query("select founder_claim_machine_id, founder_machine_id, removal_endpoints from organization_pairing")).rows[0];
    expect(row.founder_claim_machine_id).toBe(attempt.machineId);
    expect(row.founder_machine_id).toBe(phase === "join" ? attempt.machineId : null);
    expect(enrollmentSettings.encryption.decrypt(row.removal_endpoints[0].encryptedExpected)).toBe(capability);
  });

  it("opens the actual Organization runtime from one protected candidate without connecting List", async () => {
    const fake = fakeSession(harness.database);
    expect(await fake.coordinator.open()).toEqual({ status: "no_connection" });
    const attempt = await pendingFounder(fake);
    expect(await fake.coordinator.open()).toEqual({ status: "unreachable", error: null });
    await fake.coordinator.publish({ ...attempt, capability });
    expect(await fake.coordinator.open()).toMatchObject({ status: "connected" });
    expect(fake.connected).toEqual([expect.objectContaining({ connections: [{ management: capability, machine_id: attempt.machineId }] })]);
    expect(fake.closed()).toBe(1);
    fake.connection.error = new Error("candidate unreachable");
    expect(await fake.coordinator.open()).toMatchObject({ status: "unreachable", error: { _tag: "PloyzProviderError" } });
    expect(fake.observations()).toBe(0);
    expect((await harness.pool.query("select founder_machine_id from organization_pairing")).rows).toEqual([{ founder_machine_id: null }]);
  });

  it("spends a token on the first Machine identity that claims it", async () => {
    const fake = fakeSession(harness.database);
    const founder = await pendingFounder(fake);
    expect(await fake.coordinator.enroll({ token: founder.token, identity: identity(1) }))
      .toMatchObject({ failure: { _tag: "Unauthorized" } });
    await fake.coordinator.publish({ ...founder, capability });
    await fake.coordinator.completeFounding(founder);

    const token = await issueToken();
    expect(await fake.coordinator.enroll({ token, identity: identity(1) })).toMatchObject({ success: { kind: "join" } });
    for (const other of [identity(2), { ...identity(1), publicKey: identity(2).publicKey }, { ...identity(2), publicKey: identity(1).publicKey }]) {
      expect(await fake.coordinator.enroll({ token, identity: other })).toMatchObject({ failure: { _tag: "Unauthorized" } });
    }
    expect(fake.published.map((assignment) => assignment.machine.id)).toEqual([identity(1).machineId]);
    expect((await harness.pool.query("select assignments from enrollment_allocation")).rows[0].assignments).toHaveLength(1);
  });

  it("lets the claiming identity resume with its own token", async () => {
    const fake = fakeSession(harness.database);
    const founder = await pendingFounder(fake);
    expect(await fake.coordinator.enroll({ token: founder.token, identity: identity(0) }))
      .toMatchObject({ success: { kind: "initialize", resumed: true } });
    await fake.coordinator.publish({ ...founder, capability });
    await fake.coordinator.completeFounding(founder);
    const token = await issueToken();
    const first = await fake.coordinator.enroll({ token, identity: identity(1) });
    const again = await fake.coordinator.enroll({ token, identity: identity(1) });
    expect(first).toMatchObject({ success: { kind: "join" } });
    expect(again).toEqual(first);
    expect(new Set(fake.published.map((assignment) => JSON.stringify(assignment))).size).toBe(1);
  });

  it("lets exactly one of two racing identities claim a fresh token", async () => {
    const fake = fakeSession(harness.database);
    await readyFounder(fake);
    const token = await issueToken();
    const results = await Promise.all([1, 2].map((index) => fake.coordinator.enroll({ token, identity: identity(index) })));
    expect(results.filter((result) => Result.isSuccess(result) && result.success.kind === "join")).toHaveLength(1);
    expect(results.filter((result) => Result.isFailure(result))).toEqual([expect.objectContaining({ failure: expect.objectContaining({ _tag: "Unauthorized" }) })]);
    expect(fake.published).toHaveLength(1);
  });

  it("publishes only for the WireGuard key Cloud assigned the Machine", async () => {
    const fake = fakeSession(harness.database);
    const founder = await readyFounder(fake);
    expect(await fake.coordinator.enroll({ token: await issueToken(), identity: identity(1) })).toMatchObject({ success: { kind: "join" } });
    const impostor = { ...identity(1), publicKey: identity(7).publicKey };
    const stolen = await issueToken();
    expect(await fake.coordinator.enroll({ token: stolen, identity: impostor })).toMatchObject({ failure: { _tag: "Conflict" } });
    const connections = fake.connected.length;
    expect(await fake.coordinator.publish({
      token: stolen, machineId: impostor.machineId, pairingCredential: founder.pairingCredential, capability: keyed(0x77),
    })).toMatchObject({ failure: { _tag: "Conflict" } });
    expect((await harness.pool.query("select * from organization_machine where machine_id = $1", [impostor.machineId])).rowCount).toBe(0);
    expect(fake.connected).toHaveLength(connections);

    const founderImpostor = await issueToken({ ...identity(0), publicKey: identity(7).publicKey });
    expect(await fake.coordinator.publish({ ...founder, token: founderImpostor, capability: keyed(0x77) }))
      .toMatchObject({ failure: { _tag: "Conflict" } });
  });

  it("refuses publication and completion under a token another Machine claimed", async () => {
    const fake = fakeSession(harness.database);
    const founder = await pendingFounder(fake);
    for (const token of [await issueToken(identity(1)), await issueToken()]) {
      const attempt = { ...founder, token };
      expect(Result.isFailure(await fake.coordinator.publish({ ...attempt, capability }))).toBe(true);
      expect(Result.isFailure(await fake.coordinator.completeFounding(attempt))).toBe(true);
      expect(Result.isFailure(await fake.coordinator.completeFounding({ ...attempt, stage: "publish", capability }))).toBe(true);
    }
    expect((await harness.pool.query("select * from organization_machine")).rowCount).toBe(0);
    expect(fake.connected).toEqual([]);
  });

  async function joinedServer(fake: ReturnType<typeof fakeSession>, founder: Awaited<ReturnType<typeof readyFounder>>, index: number) {
    const token = await issueToken();
    expect(await fake.coordinator.enroll({ token, identity: identity(index) })).toMatchObject({ success: { kind: "join" } });
    const attempt = { token, machineId: identity(index).machineId, pairingCredential: founder.pairingCredential };
    expect(await fake.coordinator.publish({ ...attempt, capability: keyed(index) })).toMatchObject({ success: {} });
    return attempt;
  }

  it("spends a token for good once its Machine completes enrollment, keeping only the completion retry", async () => {
    const fake = fakeSession(harness.database);
    const founder = await readyFounder(fake);
    const joiner = await joinedServer(fake, founder, 1);
    expect(await fake.coordinator.completeFounding(joiner)).toMatchObject({ success: { machineId: joiner.machineId } });
    const saved = await harness.pool.query("select * from organization_machine order by machine_id");
    const assignments = fake.published.length;
    for (const [attempt, index] of [[founder, 0], [joiner, 1]] as const) {
      expect(await fake.coordinator.enroll({ token: attempt.token, identity: identity(index) }))
        .toMatchObject({ failure: { _tag: "Unauthorized" } });
      expect(await fake.coordinator.publish({ ...attempt, capability: "ployz1:rotated" }))
        .toMatchObject({ failure: { _tag: "Unauthorized" } });
      expect(await fake.coordinator.completeFounding({ ...attempt, stage: "publish", capability: "ployz1:rotated" }))
        .toMatchObject({ failure: { _tag: "Unauthorized" } });
      expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ success: { machineId: attempt.machineId } });
    }
    expect((await harness.pool.query("select * from organization_machine order by machine_id")).rows).toEqual(saved.rows);
    expect(fake.published).toHaveLength(assignments);
  });

  it("refuses a removed Server's token for enrollment, publication and completion", async () => {
    const fake = fakeSession(harness.database);
    const founder = await readyFounder(fake);
    const joiner = await joinedServer(fake, founder, 1);
    await fake.coordinator.completeFounding(joiner);
    const unfinished = await joinedServer(fake, founder, 2);
    const generation = hashEnrollmentToken(founder.pairingCredential);
    for (const [attempt, index] of [[founder, 0], [unfinished, 2]] as const) {
      expect(await fake.coordinator.dropRemoved({ organizationId, machineId: attempt.machineId, generation, removal: "reset" }))
        .toEqual({ kind: "others_remain" });
      expect(await fake.coordinator.enroll({ token: attempt.token, identity: identity(index) }))
        .toMatchObject({ failure: { _tag: "Unauthorized" } });
      expect(await fake.coordinator.publish({ ...attempt, capability: keyed(0x77) }))
        .toMatchObject({ failure: { _tag: "Unauthorized" } });
      expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ failure: { _tag: "Unauthorized" } });
    }
    expect((await harness.pool.query("select machine_id from organization_machine")).rows).toEqual([{ machine_id: joiner.machineId }]);
  });

  it("refuses a publication that waited on the pairing lock while its Server was removed", async () => {
    const fake = fakeSession(harness.database);
    const founder = await readyFounder(fake);
    const unfinished = await joinedServer(fake, founder, 2);
    const removal = await harness.pool.connect();
    try {
      await removal.query("begin");
      await removal.query("select 1 from organization_pairing where organization_id = $1 for update", [organizationId]);
      const pid = (await removal.query<{ pid: number }>("select pg_backend_pid() as pid")).rows[0]?.pid;
      const publication = fake.coordinator.publish({ ...unfinished, capability: keyed(0x77) });
      await vi.waitFor(async () => {
        expect((await harness.pool.query("select 1 from pg_stat_activity where $1 = any(pg_blocking_pids(pid))", [pid])).rowCount)
          .toBeGreaterThan(0);
      });
      await removal.query("delete from organization_machine where machine_id = $1", [unfinished.machineId]);
      await removal.query("update machine_enrollment_token set expires_at = clock_timestamp() where claimed_machine_id = $1", [
        unfinished.machineId,
      ]);
      await removal.query("commit");
      expect(await publication).toMatchObject({ failure: { _tag: "Unauthorized" } });
    } finally {
      removal.release();
    }
    expect((await harness.pool.query("select 1 from organization_machine where machine_id = $1", [unfinished.machineId])).rowCount)
      .toBe(0);
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
