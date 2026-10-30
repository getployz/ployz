import { generateKeyPairSync, sign } from "node:crypto";
import { inspect } from "node:util";
import { testConfigEnvironment } from "#/test/config-environment";
import { loadOrganizationConnections, SERVER_REACH_TIMEOUT_MS } from "#/modules/machines/connections.server";
import type { Client, ConnectOptions, EnrollmentAssignment, EnrollmentSnapshot, RuntimeWatchView } from "@ployz/sdk";
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
import { checkIn, checkinMessage } from "#/modules/machines/waiting/checkin.server";
import { rejectWaitingServers } from "#/modules/machines/waiting/reject.server";
import { hashFleetToken } from "#/modules/machines/waiting/fleet-token.server";
import { checkServerEnrollment, recordStage, endServerEnrollment } from "#/modules/machines/waiting/enrollment-run.server";
import { PostHog, type PostHogService } from "#/modules/analytics/posthog.server";
import type { SetupReport } from "#/modules/machines/enrollment";
import { disableOrganizationPairing, dropRemovedServer, revokeOrganizationPairing } from "#/modules/machines/pairing-removal.server";
import { asTestDouble } from "#/lib/test-double";
import { recoveryDigest } from "#/modules/machines/sealing-key.server";
import { readCollection } from "#/collections/read.server";
import { firstRuntimeFrame, OrganizationRuntime, OrganizationRuntimeLive, SESSION_CLOSE_CLUSTER_MS } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer, RpcError } from "#/modules/runtime/ployz.server";
import { InngestClient } from "#/modules/inngest/client";
import { GithubApi } from "#/modules/github/github-observation.api";
import { Polar } from "#/modules/billing/polar-provider.server";
import { fakeGithubApi } from "#/test/fake-github";
import { connectionAborts } from "#/test/connection-aborts";
import { CloudStore, openCloudStore } from "#/modules/config-store/store-sdk.server";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import { encodePublicError, statusForPublicError } from "#/server/public-error";
import {
  makeSecretEncryption,
  SecretEncryption,
} from "#/utils/encrypted-secret.server";

const disposeClients: Array<() => Promise<void>> = [];
/** The harness's Config Store, opened once so its tables exist. */
let store: CloudStore["Service"];
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
  const aborts = connectionAborts();
  let closed = 0;
  const connection = {
    error: null as Error | null,
    closeHangs: false,
    beforeConnect: async (_options: ConnectOptions) => {},
    beforeRegister: async (_assignment: EnrollmentAssignment) => {},
  };
  /** The Machines each Runtime Watch frame reports, as `[id, name]`. */
  const cluster = { frames: [] as Array<Array<[string, string]>>, snapshot };
  const coordinator = enrollmentTestClient(database, async (options) => {
    connected.push(options);
    aborts.watch(options.signal);
    await connection.beforeConnect(options);
    if (connection.error) throw connection.error;
    return asTestDouble<Client>()({
      close: () => {
        closed += 1;
        return connection.closeHangs ? new Promise<void>(() => {}) : Promise.resolve();
      },
      observeEnrollment: async () => { observations += 1; return cluster.snapshot; },
      register: async (assignment: EnrollmentAssignment) => {
        registerCalls += 1;
        published.push(assignment);
        await connection.beforeRegister(assignment);
        if (fail.publish) throw new Error("Lost publication response");
        return { assigned_machine: assignment.machine, visible_peers: [], target_versions: {} };
      },
      runtime: {
        watch: async function* () {
          for (const machines of cluster.frames) {
            yield asTestDouble<RuntimeWatchView>()({ machines: machines.map(([id, name]) => ({ machine: { id, name, runtime: {} } })) });
          }
        },
      },
    });
  });
  return {
    coordinator, connected, aborts, connection, published, fail, cluster,
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
  const layer = Layer.mergeAll(OrganizationRuntimeLive, Layer.succeed(CloudStore, store)).pipe(Layer.provideMerge(dependencies));
  const runtime = ManagedRuntime.make(layer);
  disposeClients.push(() => runtime.dispose());
  return {
    inngest,
    disable: (id = organizationId) => runtime.runPromise(disableOrganizationPairing(id)),
    dropRemoved: (input: Parameters<typeof dropRemovedServer>[0]) => runtime.runPromise(dropRemovedServer(input)),
    publish: (input: Parameters<typeof publishMachineEnrollment>[0]) => runtime.runPromise(
      Effect.result(publishMachineEnrollment(input)),
    ),
    reject: (id: string) => runtime.runPromise(rejectWaitingServers({ userId }, { organizationSlug: "enroll", ids: [id] })),
    checkin: (input: Parameters<typeof checkIn>[0]) => runtime.runPromise(checkIn(input)),
    check: (attemptId: string) => runtime.runPromise(checkServerEnrollment(attemptId)),
    connections: (id = organizationId) => runtime.runPromise(loadOrganizationConnections(id)),
    open: (id = organizationId) => runtime.runPromise(Effect.scoped(
      Effect.gen(function* () { return yield* (yield* OrganizationRuntime).open(id); }),
    )),
    firstFrame: (id = organizationId) => runtime.runPromise(firstRuntimeFrame(id)),
    /** Every frame of one Runtime Watch, read through to its end. */
    watchAll: (id = organizationId) => runtime.runPromise(Effect.scoped(Effect.gen(function* () {
      const session = yield* (yield* OrganizationRuntime).open(id);
      if (session.status !== "connected") throw new Error("Cluster not connected");
      const frames = yield* session.connected.watch();
      return yield* Effect.promise(async () => {
        const read: RuntimeWatchView[] = [];
        for await (const frame of frames) read.push(frame);
        return read;
      });
    }))),
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
    mintCli: () => runtime.runPromise(mintCliMachineEnrollment({ userId }, { organizationId })),
    read: (id: string, actor = userId) => runtime.runPromise(
      Effect.result(readMachineEnrollment({ userId: actor }, { organizationId, id })),
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
    const opened = await harness.runEffect(openCloudStore(harness.databaseUrl, harness.database, "test-encryption-secret"));
    store = { open: Effect.succeed(opened) };
  }, 60_000);

  afterEach(async () => {
    await Promise.all(disposeClients.splice(0).map((dispose) => dispose()));
  });

  afterAll(async () => {
    await harness.stop();
  });

  let issued = 0;
  /** An enrollment token nobody has claimed yet, or one already claimed by `claimedBy`. */
  async function issueToken(claimedBy?: ReturnType<typeof identity>, role: "founder" | "joiner" | null = null) {
    issued += 1;
    const token = `pmet_issued_${issued}`;
    await harness.pool.query(`insert into machine_enrollment_token (
      organization_id, created_by_user_id, token_hash, expires_at, claimed_machine_id, claimed_public_key, claimed_at, enrollment_role
    ) values ($1, $2, $3, now() + interval '1 day', $4, $5, case when $4::text is null then null else now() end, $6)`, [
      organizationId, userId, hashEnrollmentToken(token), claimedBy?.machineId ?? null, claimedBy?.publicKey ?? null, role,
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
          expect(minted.command).toMatch(/^curl .* && sudo ployz server add/u);
          expect(minted.token).toMatch(/^pmet_/u);
          expect(minted.command).not.toContain(minted.token);

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

  it("grants exactly one Organization founding attempt across twenty Machines with their own tokens", async () => {
    const fake = fakeSession(harness.database);
    const coordinator = fake.coordinator;
    // One token per Machine: a token claimed by the founder refuses every other Machine.
    const machineTokens = Array.from({ length: 20 }, (_, index) => tokens[index] ?? `pmet_founding_cas_${index}`);
    for (const token of machineTokens.slice(tokens.length)) {
      await harness.pool.query(
        `insert into machine_enrollment_token (organization_id, created_by_user_id, token_hash, expires_at)
         values ($1, $2, $3, now() + interval '1 day')`,
        [organizationId, userId, hashEnrollmentToken(token)],
      );
    }

    const outcomes = await Promise.all(
      machineTokens.map((token, index) => coordinator.enroll({ token, identity: identity(index) })),
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

  describe("a founding Server's Sealing Key recovery", () => {
    const sealingKey = `${"A".repeat(43)}=`;
    const backup = Buffer.concat([Buffer.from([1]), Buffer.alloc(76, 2)]).toString("base64");
    const storeOrganizationKey = (recoveryBlob: string | null) => harness.pool.query(
      "insert into organization_sealing_key (organization_id, public_key, recovery_blob) values ($1, $2, $3)",
      [organizationId, sealingKey, recoveryBlob],
    );
    const found = async (recoverySupport?: 1) => {
      const { coordinator } = fakeSession(harness.database);
      return coordinator.enroll({ token: tokens[0] ?? "", identity: recoverySupport === undefined ? identity(0) : { ...identity(0), recoverySupport } });
    };
    const recoveryOf = (outcome: Awaited<ReturnType<typeof found>>) =>
      Result.isSuccess(outcome) && outcome.success.kind === "initialize" ? outcome.success.recovery : "not initialized";

    it("is not offered to an older ployz founding an Organization with no Sealing Key", async () => {
      expect(recoveryOf(await found())).toBeUndefined();
    });

    it("asks the first Server to back up the key it makes", async () => {
      expect(recoveryOf(await found(1))).toEqual({ kind: "back_up" });
    });

    it("seeds a Server with its Organization's key and backup, and again on resume", async () => {
      await storeOrganizationKey(backup);
      const seed = { kind: "seed", expectedPublicKey: sealingKey, backup };

      expect(recoveryOf(await found(1))).toEqual(seed);
      expect(recoveryOf(await found(1))).toEqual(seed);
    });

    it("asks for a new key and its backup when no backup holds the Organization's key", async () => {
      await storeOrganizationKey(null);

      expect(recoveryOf(await found(1))).toEqual({ kind: "back_up" });
    });

    it("refuses an older ployz when its Organization has a key, and keeps no claim", async () => {
      await storeOrganizationKey(backup);

      expect(await found()).toMatchObject({
        failure: { _tag: "Conflict", message: "Update ployz on this server to restore its Cluster's Sealing Key.", userFacing: true },
      });
      expect((await harness.pool.query("select organization_id from organization_pairing")).rows).toEqual([]);
      expect((await harness.pool.query("select claimed_machine_id from machine_enrollment_token where claimed_machine_id is not null")).rows)
        .toEqual([]);
    });
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
    expect(waiter.success).toEqual({ kind: "not_yet", retryAfter: 2, reason: "founder" });
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
    // Only staging or deploying a generated domain reserves the Cluster Domain.
    expect((await harness.pool.query("select name from organization_cluster_domain")).rows).toEqual([]);

    const waiterTokens = await Promise.all(Array.from({ length: 19 }, () => issueToken()));
    const waiters = await Promise.all(
      waiterTokens.map((token, index) => coordinator.enroll({ token, identity: identity(index + 1), joinSlots: 20 })),
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
      token: await issueToken(), identity: identity(31), joinSlots: 20,
    });
    expect(indeterminate).toMatchObject({ failure: { _tag: "Conflict", message: CLUSTER_UNREACHABLE } });
    expect(fake.registerCalls()).toBe(calls);
    expect((await harness.pool.query("select founder_machine_id from organization_pairing")).rows)
      .toEqual([{ founder_machine_id: founderMachineId }]);
  });

  it("retains the committed assignment after publication failure and lets an unjoined rerun change its inputs", async () => {
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
    // The Server was registered but its daemon never ran (#3342): a rerun with other options replaces the attempt.
    fresh.cluster.snapshot = { ...snapshot, machines: [fresh.published[0]?.machine].filter((machine) => machine !== undefined) };
    const changed = await fresh.coordinator.enroll({ token: (tokens[1] ?? ""), identity: { ...identity(1), requestedStorage: "zfs" } });
    expect(changed).toMatchObject({ success: { kind: "join", storage: "zfs" } });
    expect(fresh.published).toHaveLength(2);
    expect(fresh.published[1]?.machine).toEqual(fresh.published[0]?.machine);
    expect(fresh.published[1]?.request.storage).toBe("zfs");
    const replaced = await harness.pool.query("select assignments from enrollment_allocation");
    expect(replaced.rows[0].assignments).toEqual([fresh.published[1]]);
  });

  it("refuses a joined Server's rerun with other options, and says how to proceed", async () => {
    const fake = fakeSession(harness.database);
    const founder = await pendingFounder(fake);
    await fake.coordinator.publish({ ...founder, capability });
    await fake.coordinator.completeFounding(founder);
    const first = await fake.coordinator.enroll({ token: await issueToken(), identity: identity(1) });
    const registered = fake.published[0]?.machine;
    if (!Result.isSuccess(first) || !registered) throw new Error("expected a join");
    fake.cluster.snapshot = { ...snapshot, machines: [{ ...registered, runtime: { ...registered.runtime, daemon_version: "0.9.0" } }] };
    const changed = await fake.coordinator.enroll({ token: await issueToken(), identity: { ...identity(1), requestedStorage: "zfs" } });
    expect(changed).toMatchObject({ failure: { _tag: "Conflict", userFacing: true, message: expect.stringContaining("ployz-uninstall") } });
    expect(fake.registerCalls()).toBe(1);
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

  async function waitingAttempt(token: string, stage: string, role: "founder" | "joiner") {
    const fleet = "pflt_rejected_enrollment";
    const { publicKey, privateKey } = generateKeyPairSync("ed25519");
    const key = Buffer.from(publicKey.export({ format: "jwk" }).x ?? "", "base64url").toString("hex");
    const instruction = { name: "rejected", storage: { kind: "none" }, policy: { labels: {}, acceptsBuilds: true, acceptsServices: true, acceptsIngress: false } };
    const { rows: [attempt] } = await harness.pool.query<{ id: string; waiting_server_id: string }>(`
      with fleet as (
        insert into fleet_token (organization_id, name, token_hash, expires_at, created_by_user_id)
        values ($1, 'fleet', $2, now() + interval '1 day', $3) returning id
      ), server as (
        insert into waiting_server (organization_id, fleet_token_id, checkin_key, report, observed_ip)
        select $1, id, $4, '{}'::jsonb, '203.0.113.9' from fleet returning id, fleet_token_id
      )
      insert into server_enrollment (organization_id, waiting_server_id, fleet_token_id, role, instruction,
        enroll_token_id, encrypted_token, state, inngest_run_id)
      select $1, server.id, server.fleet_token_id, $5, $6::jsonb, token.id, $7::jsonb, 'running', 'rejection-run'
      from server, machine_enrollment_token token where token.token_hash = $8
      returning id, waiting_server_id
    `, [organizationId, hashFleetToken(fleet), userId, key, role, JSON.stringify(instruction),
      JSON.stringify(enrollmentSettings.encryption.encrypt(token)), hashEnrollmentToken(token)]);
    if (attempt === undefined) throw new Error("No waiting attempt");
    await harness.pool.query("update machine_enrollment_token set server_enrollment_id = $1 where token_hash = $2", [attempt.id, hashEnrollmentToken(token)]);
    await harness.runEffect(recordStage(attempt.id, "rejection-run", stage));
    const checkin = () => {
      const body = Buffer.from(JSON.stringify({ protocolVersion: 1, checkinKey: key, cliVersion: "0.3.0",
        report: { hostname: "rejected", os: { id: "ubuntu", version: "24.04" }, kernel: "6.8", arch: "x86_64", virt: "kvm",
          cpuCount: 4, memoryTotalBytes: 8589934592, disks: [], docker: { kind: "absent" }, cgroup: "v2",
          zfs: { kind: "ineligible", code: "unavailable", message: "Unavailable" }, blockers: [], warnings: [] },
        attempt: { attemptId: attempt.id, stage },
      }));
      const timestamp = BigInt(Math.floor(Date.now() / 1000));
      const signature = sign(null, checkinMessage(key, timestamp, body), privateKey).toString("base64url");
      return { headers: new Headers({ authorization: `Bearer ${fleet}`, "ployz-timestamp": String(timestamp), "ployz-signature": `v1=${signature}` }), body, observedIp: "203.0.113.9" };
    };
    return { ...attempt, checkin };
  }

  it.each(["joining", "publishing"])("finishes a rejected %s attempt through enroll, publish and callback", async (stage) => {
    const fake = fakeSession(harness.database);
    await readyFounder(fake);
    const token = await issueToken(undefined, "joiner");
    const claimed = await fake.coordinator.enroll({ token, identity: identity(1) });
    if (Result.isFailure(claimed) || claimed.success.kind !== "join") throw new Error("No join claim");
    const attempt = await waitingAttempt(token, stage, "joiner");
    await harness.pool.query("update server_enrollment set inngest_run_id = null where id = $1", [attempt.id]);
    await fake.coordinator.reject(attempt.waiting_server_id);
    expect(await fake.coordinator.enroll({ token, identity: identity(1) })).toMatchObject({ success: { kind: "join" } });
    expect(await fake.coordinator.checkin(attempt.checkin())).toMatchObject({ kind: "enroll", token });
    expect((await harness.pool.query("select name from operation_outbox where name = 'server/enrollment.cancel'")).rows).toEqual([]);
    await harness.pool.query("update server_enrollment set inngest_run_id = 'rejection-run' where id = $1", [attempt.id]);
    const completion = { token, machineId: identity(1).machineId, pairingCredential: claimed.success.pairing.secret };
    expect(await fake.coordinator.publish({ ...completion, capability })).toMatchObject({ success: { machineId: completion.machineId } });
    expect(await fake.coordinator.completeFounding(completion)).toMatchObject({ success: { machineId: completion.machineId } });
    expect(await fake.coordinator.check(attempt.id)).toEqual({ kind: "joined", machineId: completion.machineId });
    await harness.runEffect(endServerEnrollment(attempt.id, "rejection-run", { state: "joined", machineId: completion.machineId }));
    expect(await fake.coordinator.checkin(attempt.checkin())).toMatchObject({ kind: "stop", reason: "rejected" });
  });

  it.each(["joined", "failed"] as const)("preserves a rejected committed founder until it ends %s", async (ending) => {
    const fake = fakeSession(harness.database);
    const token = await issueToken(undefined, "founder");
    const founder = await pendingFounder(fake, token);
    const attempt = await waitingAttempt(token, "joining", "founder");
    await fake.coordinator.reject(attempt.waiting_server_id);
    expect(await fake.coordinator.enroll({ token, identity: identity(0) })).toMatchObject({ success: { kind: "initialize", resumed: true } });
    expect(await fake.coordinator.checkin(attempt.checkin())).toMatchObject({ kind: "enroll", token });
    expect((await harness.pool.query("select name from operation_outbox where name = 'server/enrollment.cancel'")).rows).toEqual([]);
    if (ending === "joined") {
      expect(await fake.coordinator.publish({ ...founder, capability })).toMatchObject({ success: { machineId: founder.machineId } });
      expect(await fake.coordinator.completeFounding(founder)).toMatchObject({ success: { machineId: founder.machineId } });
      await harness.runEffect(endServerEnrollment(attempt.id, "rejection-run", { state: "joined", machineId: founder.machineId }));
      expect(await fake.coordinator.check(attempt.id)).toEqual({ kind: "joined", machineId: founder.machineId });
    } else {
      await harness.runEffect(endServerEnrollment(attempt.id, "rejection-run", { state: "failed", failure: { stage: "publishing", message: "Publish failed" } }));
      expect(await fake.coordinator.enroll({ token, identity: identity(0) })).toMatchObject({ failure: { _tag: "Unauthorized" } });
    }
    expect(await fake.coordinator.checkin(attempt.checkin())).toMatchObject({ kind: "stop", reason: "rejected" });
  });

  it.each(["installing", "enrolling", "queued", "storage"])("still cancels a rejected %s attempt and refuses its token", async (stage) => {
    const fake = fakeSession(harness.database);
    const token = await issueToken(undefined, "founder");
    const attempt = await waitingAttempt(token, stage, "founder");
    await harness.pool.query("update server_enrollment set inngest_run_id = null where id = $1", [attempt.id]);
    await fake.coordinator.reject(attempt.waiting_server_id);
    expect(await fake.coordinator.enroll({ token, identity: identity(0) })).toMatchObject({ failure: { _tag: "Unauthorized" } });
    expect((await harness.pool.query("select state from server_enrollment where id = $1", [attempt.id])).rows).toEqual([{ state: "cancelled" }]);
    expect(await fake.coordinator.checkin(attempt.checkin())).toMatchObject({ kind: "stop", reason: "rejected" });
  });

  it("refuses a cancelled attempt's token while Cloud's clock runs ahead of Postgres's", async () => {
    const fake = fakeSession(harness.database);
    const token = await issueToken(undefined, "founder");
    const attempt = await waitingAttempt(token, "installing", "founder");
    await harness.pool.query("update server_enrollment set inngest_run_id = null where id = $1", [attempt.id]);
    vi.useFakeTimers({ toFake: ["Date"], now: Date.now() + 60_000 });
    try {
      await fake.coordinator.reject(attempt.waiting_server_id);
    } finally {
      vi.useRealTimers();
    }
    expect(await fake.coordinator.enroll({ token, identity: identity(0) })).toMatchObject({ failure: { _tag: "Unauthorized" } });
  });

  it("reports the Server that completed enrollment with a CLI-minted token, and only to members", async () => {
    const fake = fakeSession(harness.database);
    const minted = await fake.coordinator.mintCli();
    expect(minted.token).toMatch(/^pmet_/u);
    expect(await fake.coordinator.read(minted.id)).toMatchObject({ success: { status: "pending" } });
    expect(await fake.coordinator.read(minted.id, "00000000-0000-4000-8000-000000000499")).toMatchObject({ failure: {} });
    const plainMember = "00000000-0000-4000-8000-000000000497";
    await harness.pool.query(`
      insert into "user" (id, email, name) values ('${plainMember}', 'member@example.com', 'Member');
      insert into member (id, organization_id, user_id, role, created_at)
      values (gen_random_uuid(), '${organizationId}', '${plainMember}', 'member', now());
    `);
    expect(await fake.coordinator.read(minted.id, plainMember)).toMatchObject({ success: { status: "pending" } });

    const attempt = await pendingFounder(fake, minted.token);
    const founder = attempt;
    await fake.coordinator.publish({ ...attempt, capability });
    expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ success: { machineId: founder.machineId } });
    expect(await fake.coordinator.read(minted.id)).toMatchObject({ success: { status: "joined", machineId: founder.machineId } });
    expect(await fake.coordinator.read(minted.id, plainMember)).toMatchObject({ success: { status: "joined", machineId: founder.machineId } });

    await harness.pool.query("update machine_enrollment_token set joined_machine_id = null, expires_at = $2 where id = $1", [minted.id, new Date(Date.now() - 1000)]);
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
    expect(fake.connected).toEqual([expect.objectContaining({ connections: [{ management: capability, machine_id: attempt.machineId, name: "node-0" }] })]);
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

  it("refreshes credentials within its Cluster time, and aborts the connection, when the Machine's session never closes", async () => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    await fake.coordinator.publish({ ...attempt, capability });
    fake.connection.closeHangs = true;
    const started = performance.now();
    expect(await fake.coordinator.publish({ ...attempt, capability: "ployz1:rotated" }))
      .toMatchObject({ success: { machineId: attempt.machineId } });
    expect(performance.now() - started).toBeLessThan(SESSION_CLOSE_CLUSTER_MS + 8_000);
    expect(fake.aborts.aborted().at(-1)).toBe(true);
  }, 30_000);

  const cantReach = expect.objectContaining({
    _tag: "Unavailable", userFacing: true, message: expect.stringContaining("through the relay or directly"),
  });

  const undialed = () => new RpcError({ code: "unavailable", message: "tcp connect error", details: null, unanswered: true });
  const unrouted = () => new RpcError({ code: "unavailable", message: "relay could not dial", details: null, targetUnreached: true });
  const refused = () => new RpcError({ code: "permission_denied", message: "identity mismatch", details: null });

  it.each([
    ["the dial fails", (fake: ReturnType<typeof fakeSession>) => { fake.connection.error = undialed(); }],
    ["the entry can't dial it", (fake: ReturnType<typeof fakeSession>) => { fake.connection.error = unrouted(); }],
    ["nothing answers in time", (fake: ReturnType<typeof fakeSession>) => { fake.connection.beforeConnect = () => new Promise(() => {}); }],
  ])("answers a refresh Cloud can't reach because %s with a retryable 503 that says why", async (_case, unreachable) => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    await fake.coordinator.publish({ ...attempt, capability });
    unreachable(fake);
    const refreshed = await fake.coordinator.publish({ ...attempt, capability: "ployz1:rotated" });
    if (!Result.isFailure(refreshed)) throw new Error("Refresh succeeded without reaching the Machine");
    expect(refreshed.failure).toEqual(cantReach);
    expect(statusForPublicError(encodePublicError(refreshed.failure))).toBe(503);
  }, 45_000);

  it("keeps a Server's refusal of a refresh, so the Server doesn't retry it as unreachable", async () => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    await fake.coordinator.publish({ ...attempt, capability });
    fake.connection.error = refused();
    const refreshed = await fake.coordinator.publish({ ...attempt, capability: "ployz1:rotated" });
    if (!Result.isFailure(refreshed)) throw new Error("Refresh succeeded past the Server's refusal");
    expect(refreshed.failure).toMatchObject({ _tag: "PloyzProviderError" });
    expect(statusForPublicError(encodePublicError(refreshed.failure))).not.toBe(503);
  });

  it("answers a completion Cloud can't reach with a retryable 503 that says why, within its reach time", async () => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    await fake.coordinator.publish({ ...attempt, capability });
    fake.connection.beforeConnect = () => new Promise(() => {});
    const started = performance.now();
    const completed = await fake.coordinator.completeFounding(attempt);
    expect(performance.now() - started).toBeLessThan(SERVER_REACH_TIMEOUT_MS + 2_000);
    if (!Result.isFailure(completed)) throw new Error("Completion succeeded without reaching the Machine");
    expect(completed.failure).toEqual(cantReach);
    expect(statusForPublicError(encodePublicError(completed.failure))).toBe(503);
  }, 45_000);

  it("doesn't call a completion that failed without the SDK's answer a refusal", async () => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    await fake.coordinator.publish({ ...attempt, capability });
    fake.connection.error = new Error("Connecting to the organization's machines timed out");
    const completed = await fake.coordinator.completeFounding(attempt);
    if (!Result.isFailure(completed)) throw new Error("Completion succeeded without reaching the Machine");
    expect(completed.failure).toEqual(cantReach);
  });

  it("names a Server's refusal of completion instead of blaming the relay", async () => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    await fake.coordinator.publish({ ...attempt, capability });
    fake.connection.error = refused();
    const completed = await fake.coordinator.completeFounding(attempt);
    if (!Result.isFailure(completed)) throw new Error("Completion succeeded past the Server's refusal");
    expect(completed.failure).toMatchObject({ _tag: "Unavailable", message: "This Server refused Cloud's connection." });
    expect(statusForPublicError(encodePublicError(completed.failure))).toBe(503);
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

  it.each([
    ["unreachable", undialed, "Unavailable"],
    ["identity mismatch", refused, "PloyzProviderError"],
  ])("preserves saved credentials when refresh verification fails: %s", async (_case, error, tag) => {
    const fake = fakeSession(harness.database);
    const attempt = await pendingFounder(fake);
    await fake.coordinator.publish({ ...attempt, capability });
    const before = await harness.pool.query("select * from organization_machine");
    fake.connection.error = error();
    expect(await fake.coordinator.publish({ ...attempt, capability: "ployz1:unverified" }))
      .toMatchObject({ failure: { _tag: tag } });
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
    expect(await fake.coordinator.completeFounding(candidate)).toMatchObject({ failure: { _tag: "Unavailable" } });
    fake.connection.error = null;
    expect(await fake.coordinator.completeFounding(candidate)).toMatchObject({ success: { machineId: candidate.machineId } });
    expect(await fake.coordinator.completeFounding(candidate)).toMatchObject({ success: { machineId: candidate.machineId } });
    expect(fake.connected.at(-1)).toMatchObject({ connections: [{ management: "ployz1:replacement", machine_id: candidate.machineId }] });
    expect((await harness.pool.query("select founder_machine_id, founder_claim_machine_id from organization_pairing")).rows)
      .toEqual([{ founder_machine_id: founder.machineId, founder_claim_machine_id: founder.machineId }]);
    await harness.pool.query("update organization_machine set is_dial_entry = (machine_id = $1)", [candidate.machineId]);
    expect(await fake.coordinator.connections()).toEqual({ kind: "ready", generation: hashEnrollmentToken(founder.pairingCredential), connections: [
      { management: "ployz1:replacement", machine_id: candidate.machineId, name: "node-1" },
      { management: capability, machine_id: founder.machineId, name: "node-0" },
    ] });
    expect(await fake.coordinator.open()).toMatchObject({ status: "connected" });
    expect(fake.connected.at(-1)).toMatchObject({ connections: [
      { management: "ployz1:replacement", machine_id: candidate.machineId },
      { management: capability, machine_id: founder.machineId },
    ] });
  });

  it("stores the name each Server enrolled with, and refreshes it from what the Cluster reports", async () => {
    const fake = fakeSession(harness.database);
    const names = async () => (await harness.pool.query<{ machine_id: string; machine_name: string | null }>(
      "select machine_id, machine_name from organization_machine order by machine_id",
    )).rows.map((row) => [row.machine_id, row.machine_name]);
    const founder = await readyFounder(fake);
    await fake.coordinator.enroll({ token: tokens[1] ?? "", identity: identity(1) });
    const joiner = { token: tokens[1] ?? "", machineId: identity(1).machineId, pairingCredential: founder.pairingCredential };
    await fake.coordinator.publish({ ...joiner, capability: "ployz1:joining" });
    // Claimed before Cloud kept names: nothing to store until the Cluster names it.
    await harness.runEffect(reserveEnrollmentAssignment({
      organizationId, pairing: founder.pairingCredential,
      identity: registerRequestFromEnrollmentIdentity(identity(2)), snapshot,
    }));
    await fake.coordinator.publish({
      ...founder, token: await issueToken(identity(2)), machineId: identity(2).machineId, capability: "ployz1:unnamed",
    });
    expect(await names()).toEqual([
      [founder.machineId, "node-0"], [joiner.machineId, "node-1"], [identity(2).machineId, null],
    ]);

    // A Server the Cluster doesn't report keeps the name Cloud has.
    fake.cluster.frames = [[[founder.machineId, "web-1"], [identity(2).machineId, "node-2"], [identity(9).machineId, "stranger"]]];
    await fake.coordinator.firstFrame();
    // The read hands its names to a write it doesn't wait on.
    await expect.poll(names).toEqual([
      [founder.machineId, "web-1"], [joiner.machineId, "node-1"], [identity(2).machineId, "node-2"],
    ]);

    fake.cluster.frames = [[[founder.machineId, "web-1"]], [[founder.machineId, "web-2"], [joiner.machineId, "db-1"]]];
    expect(await fake.coordinator.watchAll()).toHaveLength(2);
    await expect.poll(names).toEqual([
      [founder.machineId, "web-2"], [joiner.machineId, "db-1"], [identity(2).machineId, "node-2"],
    ]);
    // What Cloud dials with, a Deployment's runner included, names each Server.
    await harness.pool.query("update organization_machine set machine_name = null where machine_id = $1", [identity(2).machineId]);
    const dialed = await fake.coordinator.connections();
    expect(dialed.kind === "ready" ? dialed.connections.map(({ machine_id, name }) => [machine_id, name]) : dialed)
      .toEqual(expect.arrayContaining([
        [founder.machineId, "web-2"], [joiner.machineId, "db-1"], [identity(2).machineId, undefined],
      ]));
    expect(dialed.kind === "ready" && dialed.connections.find(({ machine_id }) => machine_id === identity(2).machineId))
      .not.toHaveProperty("name");
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
    expect(await fake.coordinator.connections()).toEqual({ kind: "ready", generation: hashEnrollmentToken(attempt.pairingCredential), connections: [{ management: capability, machine_id: attempt.machineId, name: "node-0" }] });
    expect(await fake.coordinator.connections(otherId)).toEqual({ kind: "ready", generation: hashEnrollmentToken(otherAttempt.pairingCredential), connections: [{ management: "ployz1:other", machine_id: attempt.machineId, name: "node-0" }] });
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
    expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ failure: { _tag: "Unavailable" } });
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

  const sealingKeys = () => harness.pool.query<{ cluster_key: string; public_key: string }>(
    "select cluster_key, public_key from cluster_sealing_key where organization_id = $1", [organizationId],
  ).then((result) => result.rows);
  const held = (letter: string) => ({ kind: "held" as const, publicKey: `${letter.repeat(43)}=` });
  /** Effect's default logger writes every level through `console.log`, naming the level before the message. */
  const logged = (level: "WARN" | "ERROR", text: string) => {
    const spy = vi.spyOn(console, "log");
    return {
      count: () => spy.mock.calls.map((args) => inspect(args, { depth: null })).filter((line) => line.includes(` ${level} `) && line.includes(text)).length,
      [Symbol.dispose]: () => spy.mockRestore(),
    };
  };

  it("keeps the first Sealing Key its pairing reports and logs a different one as an error", async () => {
    const fake = fakeSession(harness.database);
    const founder = await pendingFounder(fake);
    await fake.coordinator.publish({ ...founder, capability });
    expect(await fake.coordinator.completeFounding({ ...founder, sealing: held("A") })).toMatchObject({ success: { machineId: founder.machineId } });
    expect(await fake.coordinator.completeFounding({ ...founder, sealing: held("A") })).toMatchObject({ success: { machineId: founder.machineId } });
    const recorded = [{ cluster_key: hashEnrollmentToken(founder.pairingCredential), public_key: held("A").publicKey }];
    expect(await sealingKeys()).toEqual(recorded);

    await fake.coordinator.enroll({ token: tokens[1] ?? "", identity: identity(1) });
    const joiner = { token: tokens[1] ?? "", machineId: identity(1).machineId, pairingCredential: founder.pairingCredential };
    await fake.coordinator.publish({ ...joiner, capability: "ployz1:joining" });
    using errors = logged("ERROR", "unlike the one its Cluster first reported");
    expect(await fake.coordinator.completeFounding({ ...joiner, sealing: held("B") })).toMatchObject({ success: { machineId: joiner.machineId } });
    expect(await sealingKeys()).toEqual(recorded);
    expect(errors.count()).toBe(1);
  });

  it("records no Sealing Key for a Server that joined without one, and warns", async () => {
    const fake = fakeSession(harness.database);
    const founder = await pendingFounder(fake);
    await fake.coordinator.publish({ ...founder, capability });
    using warnings = logged("WARN", "joined without its Cluster's Sealing Key");
    expect(await fake.coordinator.completeFounding({ ...founder, sealing: { kind: "needs_access" } }))
      .toMatchObject({ success: { machineId: founder.machineId } });
    expect(await sealingKeys()).toEqual([]);
    expect(warnings.count()).toBe(1);
  });

  it("drops the Sealing Key of a completion its Organization re-paired under", async () => {
    const fake = fakeSession(harness.database);
    const founder = await readyFounder(fake);
    await fake.coordinator.enroll({ token: tokens[1] ?? "", identity: identity(1) });
    const joiner = { token: tokens[1] ?? "", machineId: identity(1).machineId, pairingCredential: founder.pairingCredential };
    await fake.coordinator.publish({ ...joiner, capability: "ployz1:joining" });
    fake.connection.beforeConnect = async () => {
      await harness.pool.query("update organization_pairing set encrypted_pairing_secret = $1::jsonb where organization_id = $2", [
        JSON.stringify(enrollmentSettings.encryption.encrypt("ppair_replacement")), organizationId,
      ]);
    };
    expect(await fake.coordinator.completeFounding({ ...joiner, sealing: held("B") })).toMatchObject({ failure: { _tag: "Conflict" } });
    expect(await sealingKeys()).toEqual([]);
  });

  describe("the founder's recovery backup", () => {
    const backup = (fill: number) => Buffer.concat([Buffer.from([1]), Buffer.alloc(76, fill)]).toString("base64");
    const organizationKey = () => harness.pool.query<{ public_key: string; recovery_blob: string | null }>(
      "select public_key, recovery_blob from organization_sealing_key where organization_id = $1", [organizationId],
    ).then((result) => result.rows);
    const storeOrganizationKey = (letter: string, recoveryBlob: string | null) => harness.pool.query(
      "insert into organization_sealing_key (organization_id, public_key, recovery_blob) values ($1, $2, $3)",
      [organizationId, held(letter).publicKey, recoveryBlob],
    );
    const founderMachine = () => harness.pool.query<{ founder_machine_id: string | null }>("select founder_machine_id from organization_pairing")
      .then((result) => result.rows);
    const completeWithBackup = async (recoveryBackup: { publicKey: string; backup: string }) => {
      const fake = fakeSession(harness.database);
      const founder = await pendingFounder(fake);
      await fake.coordinator.publish({ ...founder, capability });
      const complete = () => fake.coordinator.completeFounding({ ...founder, sealing: held("A"), recoveryBackup });
      return { founder, complete };
    };

    it("is stored with the key, echoed as its digest, and accepted again on retry", async () => {
      const { founder, complete } = await completeWithBackup({ publicKey: held("A").publicKey, backup: backup(2) });
      const completed = { success: { machineId: founder.machineId, recoveryBackupDigest: recoveryDigest(backup(2)) } };

      expect(await complete()).toMatchObject(completed);
      expect(await complete()).toMatchObject(completed);
      expect(await organizationKey()).toEqual([{ public_key: held("A").publicKey, recovery_blob: backup(2) }]);
    });

    it("fills the Organization's recorded key that has no backup", async () => {
      const { complete } = await completeWithBackup({ publicKey: held("A").publicKey, backup: backup(2) });
      await storeOrganizationKey("A", null);

      expect(await complete()).toMatchObject({ success: { recoveryBackupDigest: recoveryDigest(backup(2)) } });
      expect(await organizationKey()).toEqual([{ public_key: held("A").publicKey, recovery_blob: backup(2) }]);
    });

    it("replaces another key that has no backup", async () => {
      const { complete } = await completeWithBackup({ publicKey: held("A").publicKey, backup: backup(2) });
      await storeOrganizationKey("B", null);

      expect(await complete()).toMatchObject({ success: { recoveryBackupDigest: recoveryDigest(backup(2)) } });
      expect(await organizationKey()).toEqual([{ public_key: held("A").publicKey, recovery_blob: backup(2) }]);
    });

    it("refuses another backup or another backed-up key, writes nothing and leaves the founder uncommitted", async () => {
      for (const [letter, stored] of [["A", backup(3)], ["B", backup(2)]] as const) {
        await harness.pool.query("delete from organization_sealing_key");
        const { complete } = await completeWithBackup({ publicKey: held("A").publicKey, backup: backup(2) });
        await storeOrganizationKey(letter, stored);

        expect(await complete()).toMatchObject({ failure: { _tag: "Conflict" } });
        expect(await organizationKey()).toEqual([{ public_key: held(letter).publicKey, recovery_blob: stored }]);
        expect(await sealingKeys()).toEqual([]);
        expect(await founderMachine()).toEqual([{ founder_machine_id: null }]);
      }
    });

    it("is refused when it backs up a key other than the one the Server holds", async () => {
      const { complete } = await completeWithBackup({ publicKey: held("B").publicKey, backup: backup(2) });

      expect(await complete()).toMatchObject({ failure: { _tag: "Validation" } });
      expect(await organizationKey()).toEqual([]);
    });

    it("is refused as any callback for a token Cloud doesn't know", async () => {
      const { founder } = await completeWithBackup({ publicKey: held("A").publicKey, backup: backup(2) });
      const fake = fakeSession(harness.database);
      const recoveryBackup = { publicKey: held("A").publicKey, backup: backup(2) };

      expect(await fake.coordinator.completeFounding({ ...founder, token: "pmet_unknown", sealing: held("A"), recoveryBackup }))
        .toMatchObject({ failure: { _tag: "Unauthorized" } });
    });
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
    expect(await pending).toMatchObject({ failure: { _tag: phase === "join" ? "Conflict" : "Unavailable" } });
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
    expect(fake.connected).toEqual([expect.objectContaining({ connections: [{ management: capability, machine_id: attempt.machineId, name: "node-0" }] })]);
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

  it("answers a rerun for a Server that already joined from its own row, without registering it again", async () => {
    const fake = fakeSession(harness.database);
    const founder = await pendingFounder(fake);
    await fake.coordinator.publish({ ...founder, capability });
    await fake.coordinator.completeFounding(founder);
    const token = await issueToken();
    const first = await fake.coordinator.enroll({ token, identity: identity(1) });
    const registered = fake.published[0]?.machine;
    if (!Result.isSuccess(first) || first.success.kind !== "join" || !registered) throw new Error("expected a join");
    // The Server joined and published its own row; its publish callback to Cloud was lost.
    const joined = { ...registered, runtime: { ...registered.runtime, daemon_version: "0.9.0" } };
    fake.cluster.snapshot = { ...snapshot, machines: [joined], target_versions: { machines: 7 } };
    for (const rerun of [token, await issueToken()]) {
      expect(await fake.coordinator.enroll({ token: rerun, identity: identity(1) })).toMatchObject({
        success: { kind: "join", registration: { assigned_machine: joined, visible_peers: [], target_versions: { machines: 7 } } },
      });
    }
    expect(fake.registerCalls()).toBe(1);
  });

  it("never lets a joiner token take or resume the Founding Claim", async () => {
    const fake = fakeSession(harness.database);
    const joiner = await issueToken(undefined, "joiner");
    expect(await fake.coordinator.enroll({ token: joiner, identity: identity(0) })).toMatchObject({ success: { kind: "not_yet" } });
    expect((await harness.pool.query("select 1 from organization_pairing")).rowCount).toBe(0);

    expect(await fake.coordinator.enroll({ token: tokens[0] ?? "", identity: identity(0) }))
      .toMatchObject({ success: { kind: "initialize", resumed: false } });
    expect(await fake.coordinator.enroll({ token: joiner, identity: identity(0) })).toMatchObject({ success: { kind: "not_yet" } });
    expect((await harness.pool.query("select claimed_machine_id from machine_enrollment_token where enrollment_role = 'joiner'")).rows)
      .toEqual([{ claimed_machine_id: null }]);
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

  it("resumes a founder Cloud still lists, and joins one it forgot like any other Server", async () => {
    const fake = fakeSession(harness.database);
    const founder = await readyFounder(fake);
    const joiner = await joinedServer(fake, founder, 1);
    await fake.coordinator.completeFounding(joiner);
    expect(await fake.coordinator.enroll({ token: await issueToken(), identity: identity(0) }))
      .toMatchObject({ success: { kind: "initialize", resumed: true } });

    const generation = hashEnrollmentToken(founder.pairingCredential);
    expect(await fake.coordinator.dropRemoved({ organizationId, machineId: founder.machineId, generation, removal: "unconfirmed" }))
      .toEqual({ kind: "others_remain" });
    const registered = fake.registerCalls();
    expect(await fake.coordinator.enroll({ token: await issueToken(), identity: identity(0) })).toMatchObject({
      success: { kind: "join", registration: { assigned_machine: { id: founder.machineId } } },
    });
    expect(fake.registerCalls()).toBe(registered + 1);
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

  describe("join line behind the founder", () => {
    const enrollIn = (fake: ReturnType<typeof fakeSession>, token: string, index: number) =>
      fake.coordinator.enroll({ token, identity: identity(index), joinSlots: 1 });
    const queued = (ahead: number) => ({ success: { kind: "not_yet", retryAfter: 5, reason: "queued", ahead } });

    it("lets one join at a time and admits the line in order as joins complete", async () => {
      const fake = fakeSession(harness.database);
      const founder = await readyFounder(fake);
      const [first, second, third] = [await issueToken(undefined, "joiner"), await issueToken(undefined, "joiner"), await issueToken()];

      expect(await enrollIn(fake, first, 1)).toMatchObject({ success: { kind: "join" } });
      expect(await enrollIn(fake, second, 2)).toEqual(queued(1));
      expect(await enrollIn(fake, third, 3)).toEqual(queued(2));
      expect(await enrollIn(fake, third, 3)).toEqual(queued(2));
      expect(await enrollIn(fake, second, 2)).toEqual(queued(1));

      const attempt = { token: first, machineId: identity(1).machineId, pairingCredential: founder.pairingCredential };
      await fake.coordinator.publish({ ...attempt, capability: keyed(1) });
      expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ success: { machineId: attempt.machineId } });

      expect(await enrollIn(fake, third, 3)).toEqual(queued(1));
      expect(await enrollIn(fake, second, 2)).toMatchObject({ success: { kind: "join" } });
      expect(await enrollIn(fake, third, 3)).toEqual(queued(1));
    });

    it("answers completion with a retryable 503 while the joining Machine is unreachable, then completes", async () => {
      const fake = fakeSession(harness.database);
      const founder = await readyFounder(fake);
      const token = await issueToken();
      expect(await enrollIn(fake, token, 1)).toMatchObject({ success: { kind: "join" } });
      const attempt = { token, machineId: identity(1).machineId, pairingCredential: founder.pairingCredential };
      await fake.coordinator.publish({ ...attempt, capability: keyed(1) });

      fake.connection.error = new Error("Candidate unreachable");
      const unreachable = await fake.coordinator.completeFounding(attempt);
      if (!Result.isFailure(unreachable)) throw new Error("Completion succeeded without reaching the Machine");
      expect(statusForPublicError(encodePublicError(unreachable.failure))).toBe(503);

      fake.connection.error = null;
      expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ success: { machineId: attempt.machineId } });
    });

    it("never gates a Machine resuming its own claim", async () => {
      const fake = fakeSession(harness.database);
      await readyFounder(fake);
      await issueToken(identity(4), "joiner");
      const resumed = await issueToken(identity(5), "joiner");
      expect(await enrollIn(fake, resumed, 5)).toMatchObject({ success: { kind: "join" } });
    });

    it("drops a waiter that stopped asking and a claim that never completed", async () => {
      const fake = fakeSession(harness.database);
      await readyFounder(fake);
      const [first, second, third] = [await issueToken(), await issueToken(), await issueToken()];
      expect(await enrollIn(fake, first, 1)).toMatchObject({ success: { kind: "join" } });
      expect(await enrollIn(fake, second, 2)).toEqual(queued(1));
      expect(await enrollIn(fake, third, 3)).toEqual(queued(2));

      await harness.pool.query("update machine_enrollment_token set updated_at = now() - interval '1 minute' where token_hash = $1", [hashEnrollmentToken(second)]);
      expect(await enrollIn(fake, third, 3)).toEqual(queued(1));

      await harness.pool.query("update machine_enrollment_token set claimed_at = now() - interval '11 minutes' where token_hash = $1", [hashEnrollmentToken(first)]);
      expect(await enrollIn(fake, third, 3)).toMatchObject({ success: { kind: "join" } });
    });

    it("gives back the slots of joins that failed before they were sent", async () => {
      const fake = fakeSession(harness.database);
      await readyFounder(fake);
      fake.connection.error = new Error("Candidate unreachable");
      for (let index = 1; index <= 8; index += 1) {
        expect(await fake.coordinator.enroll({ token: await issueToken(), identity: identity(index) }))
          .toMatchObject({ failure: { _tag: "Conflict" } });
      }
      fake.connection.error = null;
      expect(await fake.coordinator.enroll({ token: await issueToken(), identity: identity(20) }))
        .toMatchObject({ success: { kind: "join" } });
    });

    const reportFailure = (token: string, failedStep: "install" | "enroll" | "storage" | "join") =>
      Effect.runPromise(recordMachineSetupReport({
        token,
        report: {
          outcome: "failed", profile: {}, steps: [], totalSeconds: 1,
          failedStep, failedStepSeconds: 1, error: "refused",
        },
      }).pipe(
        Effect.provideService(Database, harness.database),
        Effect.provideService(PostHog, {
          capture: () => Effect.void,
          identify: () => Effect.void,
          identifyOrganization: () => Effect.void,
          toolCall: () => Effect.void,
        }),
      ));

    // A plain add of a Server that still participates is Registered, then refused by the CLI
    // before it tells the Server to join (#3630).
    it("gives back the slot of an add its CLI refused before telling the Server to join", async () => {
      const fake = fakeSession(harness.database);
      await readyFounder(fake);
      const [refused, next] = [await issueToken(), await issueToken()];
      expect(await enrollIn(fake, refused, 1)).toMatchObject({ success: { kind: "join" } });
      expect(await enrollIn(fake, next, 2)).toEqual(queued(1));

      await reportFailure(refused, "storage");
      expect(await enrollIn(fake, next, 2)).toMatchObject({ success: { kind: "join" } });
    });

    it("keeps the slot of an add that failed after telling the Server to join", async () => {
      const fake = fakeSession(harness.database);
      await readyFounder(fake);
      const [joining, next] = [await issueToken(), await issueToken()];
      expect(await enrollIn(fake, joining, 1)).toMatchObject({ success: { kind: "join" } });

      await reportFailure(joining, "join");
      expect(await enrollIn(fake, next, 2)).toEqual(queued(1));
    });

    // A report carries no identity: one that failed before Cloud answered its enroll, as another
    // Server's refused use of a claimed token does, may not be the claimant's.
    it("keeps the claimant's slot when a report fails before Cloud answered its enroll", async () => {
      const fake = fakeSession(harness.database);
      await readyFounder(fake);
      const [claimed, next] = [await issueToken(), await issueToken()];
      expect(await enrollIn(fake, claimed, 1)).toMatchObject({ success: { kind: "join" } });
      expect(await enrollIn(fake, claimed, 3)).not.toMatchObject({ success: expect.anything() });

      await reportFailure(claimed, "install");
      await reportFailure(claimed, "enroll");
      expect(await enrollIn(fake, next, 2)).toEqual(queued(1));
      expect(await enrollIn(fake, claimed, 1)).toMatchObject({ success: { kind: "join" } });
    });

    // Accepted for 0.3.11: Cloud keeps no record that a claim was answered before, so a rerun of a
    // token whose Server is already joining, refused at `storage` (say for a changed policy), frees
    // that join's slot. The join still completes; the line admits one more than its slots.
    it("lets one more through when a rerun of a still-joining add fails at storage", async () => {
      const fake = fakeSession(harness.database);
      await readyFounder(fake);
      const [joining, next] = [await issueToken(), await issueToken()];
      expect(await enrollIn(fake, joining, 1)).toMatchObject({ success: { kind: "join" } });
      expect(await enrollIn(fake, joining, 1)).toMatchObject({ success: { kind: "join" } });

      await reportFailure(joining, "storage");
      expect(await enrollIn(fake, next, 2)).toMatchObject({ success: { kind: "join" } });
    });

    it("sends a resumed claim whose slot lapsed back into the line", async () => {
      const fake = fakeSession(harness.database);
      await readyFounder(fake);
      const first = await issueToken();
      expect(await enrollIn(fake, first, 1)).toMatchObject({ success: { kind: "join" } });
      await harness.pool.query("update machine_enrollment_token set claimed_at = now() - interval '11 minutes' where token_hash = $1", [hashEnrollmentToken(first)]);
      expect(await enrollIn(fake, await issueToken(), 2)).toMatchObject({ success: { kind: "join" } });
      expect(await enrollIn(fake, first, 1)).toEqual(queued(1));
    });

    const lineKeepsItsPlace = async (fake: ReturnType<typeof fakeSession>, founder: Awaited<ReturnType<typeof readyFounder>>, first: string) => {
      const second = await issueToken();
      expect(await enrollIn(fake, second, 2)).toMatchObject({ success: { kind: "join" } });
      expect(await enrollIn(fake, first, 1)).toEqual(queued(1));
      const attempt = { token: second, machineId: identity(2).machineId, pairingCredential: founder.pairingCredential };
      await fake.coordinator.publish({ ...attempt, capability: keyed(2) });
      expect(await fake.coordinator.completeFounding(attempt)).toMatchObject({ success: { machineId: attempt.machineId } });
      expect(await enrollIn(fake, await issueToken(), 3)).toEqual(queued(1));
      expect(await enrollIn(fake, first, 1)).toMatchObject({ success: { kind: "join" } });
    };

    it("keeps a claimant whose join failed before it was sent ahead of later arrivals", async () => {
      const fake = fakeSession(harness.database);
      const founder = await readyFounder(fake);
      const first = await issueToken();
      fake.connection.error = new Error("Candidate unreachable");
      expect(await enrollIn(fake, first, 1)).toMatchObject({ failure: { _tag: "Conflict" } });
      fake.connection.error = null;
      await lineKeepsItsPlace(fake, founder, first);
    });

    it("keeps a claimant whose slot lapsed ahead of later arrivals", async () => {
      const fake = fakeSession(harness.database);
      const founder = await readyFounder(fake);
      const first = await issueToken();
      expect(await enrollIn(fake, first, 1)).toMatchObject({ success: { kind: "join" } });
      await harness.pool.query("update machine_enrollment_token set claimed_at = now() - interval '11 minutes' where token_hash = $1", [hashEnrollmentToken(first)]);
      await lineKeepsItsPlace(fake, founder, first);
    });

    it("keeps the slot of a sent join when a later retry fails before sending", async () => {
      const fake = fakeSession(harness.database);
      await readyFounder(fake);
      const first = await issueToken();
      expect(await enrollIn(fake, first, 1)).toMatchObject({ success: { kind: "join" } });
      fake.connection.error = new Error("Candidate unreachable");
      expect(await enrollIn(fake, first, 1)).toMatchObject({ failure: { _tag: "Conflict" } });
      fake.connection.error = null;
      expect(await enrollIn(fake, await issueToken(), 2)).toEqual(queued(1));
    });

    it("keeps the slot of a sent join when an earlier duplicate fails after it", async () => {
      const fake = fakeSession(harness.database);
      await readyFounder(fake);
      const first = await issueToken();
      let stalled!: () => void;
      let resume!: () => void;
      const entered = new Promise<void>((resolve) => { stalled = resolve; });
      const resumed = new Promise<void>((resolve) => { resume = resolve; });
      let connects = 0;
      fake.connection.beforeConnect = async () => {
        connects += 1;
        if (connects > 1) return;
        stalled();
        await resumed;
        throw new Error("Candidate unreachable");
      };
      const original = enrollIn(fake, first, 1);
      await entered;
      expect(await enrollIn(fake, first, 1)).toMatchObject({ success: { kind: "join" } });
      resume();
      expect(await original).toMatchObject({ failure: { _tag: "Conflict" } });
      expect(await enrollIn(fake, await issueToken(), 2)).toEqual(queued(1));
    });

    it("keeps the slot of a join that is still publishing past the idle limit", async () => {
      const fake = fakeSession(harness.database);
      const founder = await readyFounder(fake);
      const first = await issueToken();
      expect(await enrollIn(fake, first, 1)).toMatchObject({ success: { kind: "join" } });
      const age = (minutes: number) => harness.pool.query(
        `update machine_enrollment_token set claimed_at = claimed_at - interval '${minutes} minutes' where token_hash = $1`,
        [hashEnrollmentToken(first)],
      );
      await age(9);
      await fake.coordinator.publish({ token: first, machineId: identity(1).machineId, pairingCredential: founder.pairingCredential, capability: keyed(1) });
      await age(2);
      expect(await enrollIn(fake, await issueToken(), 2)).toEqual(queued(1));
    });

    it("does not count the founder's own token as a join", async () => {
      const fake = fakeSession(harness.database);
      await readyFounder(fake);
      await harness.pool.query("update machine_enrollment_token set joined_machine_id = null where claimed_machine_id = $1", [founderMachineId]);
      expect(await enrollIn(fake, await issueToken(), 1)).toMatchObject({ success: { kind: "join" } });
    });

    it("answers a full line without waiting on the Organization's lock", async () => {
      const fake = fakeSession(harness.database);
      await readyFounder(fake);
      await issueToken(identity(4), "joiner");
      const waiting = await issueToken();
      const holder = await harness.pool.connect();
      try {
        await holder.query("begin");
        await holder.query("select 1 from organization_pairing where organization_id = $1 for update", [organizationId]);
        expect(await enrollIn(fake, waiting, 5)).toEqual(queued(1));
      } finally {
        await holder.query("rollback");
        holder.release();
      }
    });

    it("counts a queued Waiting Server's ask as a check-in", async () => {
      const fake = fakeSession(harness.database);
      await readyFounder(fake);
      await issueToken(identity(4), "joiner");
      const waiting = await issueToken(undefined, "joiner");
      const { rows: [attempt] } = await harness.pool.query<{ id: string }>(`
        with fleet as (
          insert into fleet_token (organization_id, name, token_hash, expires_at, created_by_user_id)
          values ($1, 'fleet', 'fleet-hash', now() + interval '1 day', $2) returning id
        ), server as (
          insert into waiting_server (organization_id, fleet_token_id, checkin_key, report, observed_ip, last_check_in_at)
          select $1, id, 'key', '{}'::jsonb, '203.0.113.9', now() - interval '6 minutes' from fleet returning id, fleet_token_id
        )
        insert into server_enrollment (organization_id, waiting_server_id, fleet_token_id, role, instruction, enroll_token_id, state)
        select $1, server.id, server.fleet_token_id, 'joiner', '{}'::jsonb, token.id, 'running'
        from server, machine_enrollment_token token where token.token_hash = $3
        returning id
      `, [organizationId, userId, hashEnrollmentToken(waiting)]);
      if (!attempt) throw new Error("No Server Enrollment");
      await harness.pool.query("update machine_enrollment_token set server_enrollment_id = $1 where token_hash = $2", [attempt.id, hashEnrollmentToken(waiting)]);
      expect(await fake.coordinator.check(attempt.id)).toEqual({ kind: "silent" });

      expect(await enrollIn(fake, waiting, 5)).toEqual(queued(1));
      expect(await fake.coordinator.check(attempt.id)).toEqual({ kind: "waiting" });
    });
  });
});
