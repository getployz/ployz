import type { EnrollmentAssignment, EnrollmentSnapshot } from "@ployz/sdk";
import { registerRequestFromEnrollmentIdentity, rustMachineIdSchema } from "./enrollment";
import { ConfigProvider, Effect, Exit, Layer, Result, Schema } from "effect";
import { Inngest } from "inngest";
import {
  afterAll,
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
  enrollMachine,
  registerThroughHeldList,
  reserveEnrollmentAssignment,
  EnrollmentRelay,
  hashEnrollmentToken,
  heldMachineIds,
  loadOrganizationEnrollmentStatus,
  mintMachineEnrollment,
  resetPendingOrganizationEnrollment,
  tryRevokeOrganizationRelayPairing,
  type EnrollmentRelayService,
} from "#/modules/machines/enrollment.server";
import { makePloyzLayer, PloyzProviderError } from "#/modules/runtime/ployz.server";
import { InngestClient } from "#/modules/inngest/client";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import {
  makeSecretEncryption,
  SecretEncryption,
} from "#/utils/encrypted-secret.server";

const organizationId = "00000000-0000-4000-8000-000000000401";
const userId = "00000000-0000-4000-8000-000000000402";
const tokens = ["pmet_founding_cas_a", "pmet_founding_cas_b"];
const heldMachineId = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const [heldMachine] = heldMachineIds([{ machineId: heldMachineId }]);
if (heldMachine === undefined) throw new Error("Invalid test Machine id.");
const registration = {
  assigned_machine: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  visible_peers: [heldMachineId],
  target_versions: {},
};
const snapshot: EnrollmentSnapshot = { network: "10.42.0.0/16", machines: [], target_versions: {} };
const enrollmentSettings = {
  publicRelayUrl: "https://relay.example.test/",
  deploymentDialBearer: "pdial_test",
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

function fakeRelay(database: GithubPostgresTestHarness["database"]) {
  let listCalls = 0;
  let registerCalls = 0;
  const revokedPairings: string[] = [];
  const held: unknown[] = [];
  const fail = { list: false, revoke: false, publish: false, conflict: false };
  const published: EnrollmentAssignment[] = [];
  const inspectHolding: EnrollmentRelayService["inspectHolding"] = () =>
    Effect.gen(function* () {
      listCalls += 1;
      if (fail.list) {
        return yield* new PloyzProviderError({
          operation: "list held registers",
          cause: new Error("Relay unreachable"),
        });
      }
      if (held.length === 0) return { kind: "empty" as const };
      const machineIds = heldMachineIds(held);
      if (machineIds.length === 0) {
        return yield* new PloyzProviderError({
          operation: "list held registers",
          cause: new Error("Relay returned no usable Machine ids"),
        });
      }
      return { kind: "held" as const, held: machineIds };
    });
  const relay: EnrollmentRelayService = {
    inspectHolding,
    registerAvailable: (input) => registerThroughHeldList(input).pipe(
      Effect.provideService(Database, database),
      Effect.provide(makePloyzLayer({
        connect: async () => { throw new Error("unused"); },
        observeEnrollment: async () => snapshot,
        publishEnrollment: async (_url, _bearer, _pairing, _entry, assignment) => {
          registerCalls += 1;
          published.push(assignment);
          if (fail.conflict) throw Object.assign(new Error("Assignment conflicts"), { code: "conflict" });
          if (fail.publish) throw new Error("Lost publication response");
          return { assigned_machine: assignment.machine, visible_peers: [], target_versions: {} };
        },
      })),
    ),
    revokeIfEmpty: (input) =>
      Effect.gen(function* () {
        const holding = yield* inspectHolding(input);
        if (holding.kind === "held") return "held" as const;
        if (fail.revoke) {
          return yield* new PloyzProviderError({
            operation: "revoke relay pairing",
            cause: new Error("Relay revoke failed"),
          });
        }
        revokedPairings.push(input.pairing);
        return "revoked" as const;
      }),
    revokePairing: (input) =>
      Effect.gen(function* () {
        if (fail.revoke) {
          return yield* new PloyzProviderError({
            operation: "revoke relay pairing",
            cause: new Error("Relay revoke failed"),
          });
        }
        revokedPairings.push(input.pairing);
      }),
  };
  const coordinator = enrollmentTestClient(database, relay);
  return {
    coordinator,
    published,
    held,
    fail,
    revokedPairings,
    listCalls: () => listCalls,
    registerCalls: () => registerCalls,
  };
}

function enrollmentTestClient(
  database: GithubPostgresTestHarness["database"],
  relay: EnrollmentRelayService,
) {
  const provider = ConfigProvider.fromEnv({
    env: {
      NODE_ENV: "test",
      DATABASE_URL: "postgres://unused",
      ELECTRIC_URL: "http://localhost:30000",
      APP_URL: "https://cloud.example.test",
      BETTER_AUTH_SECRET: "better-auth-secret",
      GITHUB_CLIENT_ID: "github-client-id",
      GITHUB_CLIENT_SECRET: "github-client-secret",
      PLOYZ_RELAY_URL: enrollmentSettings.publicRelayUrl,
      PLOYZ_RELAY_DIAL_CREDENTIAL:
        enrollmentSettings.deploymentDialBearer ?? "",
      APP_ENCRYPTION_SECRET:
        "app-encryption-secret-at-least-32-characters",
    },
  });
  const config = AppConfig.layer.pipe(
    Layer.provide(ConfigProvider.layer(provider)),
  );
  const layer = Layer.mergeAll(
    config,
    Layer.succeed(Database, database),
    Layer.succeed(EnrollmentRelay, relay),
    Layer.succeed(InngestClient, new Inngest({ id: "enrollment-test" })),
    Layer.succeed(SecretEncryption, enrollmentSettings.encryption),
  );
  return {
    enroll: (input: Parameters<typeof enrollMachine>[0]) =>
      Effect.runPromise(
        Effect.result(enrollMachine(input).pipe(Effect.provide(layer))),
      ),
    completeFounding: (input: Parameters<typeof completeMachineEnrollment>[0]) =>
      Effect.runPromise(
        Effect.result(
          completeMachineEnrollment(input).pipe(Effect.provide(layer)),
        ),
      ),
    resetPendingEnrollment: (_organizationId: string) =>
      Effect.runPromise(
        Effect.result(
          resetPendingOrganizationEnrollment(
            { userId },
            {
              organizationSlug: "enroll",
              confirmedFounderStoppedOrErased: true,
            },
          ).pipe(Effect.provide(layer)),
        ),
      ),
    tryRevokePairing: (organizationId: string) =>
      Effect.runPromise(
        tryRevokeOrganizationRelayPairing(organizationId).pipe(
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
        ELECTRIC_URL: "http://localhost:30000",
        APP_URL: "https://cloud.example.test",
        BETTER_AUTH_SECRET: "better-auth-secret",
        GITHUB_CLIENT_ID: "github-client-id",
        GITHUB_CLIENT_SECRET: "github-client-secret",
        PLOYZ_RELAY_URL: "https://relay.example.test",
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
    const fake = fakeRelay(harness.database);
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
    expect(fake.listCalls()).toBe(0);
  });

  it("resumes only the matching founder forever and parks every other Machine without Relay", async () => {
    const fake = fakeRelay(harness.database);
    fake.fail.list = true;
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
    expect(fake.listCalls()).toBe(0);
  });

  it("commits ready only after Relay holds the founder, then joins all waiters", async () => {
    const fake = fakeRelay(harness.database);
    const coordinator = fake.coordinator;
    const first = await coordinator.enroll({ token: tokens[0] ?? "", identity: identity(0) });
    expect(Result.isFailure(first)).toBe(false);
    if (Result.isFailure(first) || first.success.kind !== "initialize") return;

    const beforeHeld = await coordinator.completeFounding({
      token: tokens[1] ?? "",
      machineId: heldMachineId,
      pairingCredential: first.success.pairing.secret,
    });
    expect(Result.isFailure(beforeHeld)).toBe(true);

    fake.held.push({ machineId: heldMachineId });
    const completed = await coordinator.completeFounding({
      token: tokens[1] ?? "",
      machineId: heldMachineId,
      pairingCredential: first.success.pairing.secret,
    });
    const repeated = await coordinator.completeFounding({
      token: tokens[0] ?? "",
      machineId: heldMachineId,
      pairingCredential: first.success.pairing.secret,
    });
    expect(Result.isFailure(completed)).toBe(false);
    expect(Result.isFailure(repeated)).toBe(false);
    if (Result.isFailure(completed) || Result.isFailure(repeated)) return;
    expect(completed.success).toEqual({ machineId: heldMachineId });
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
    expect(state.rows[0]?.founder_machine_id).toBe(heldMachineId);

    fake.held.length = 0;
    const empty = await coordinator.enroll({
      token: tokens[0] ?? "",
      identity: identity(30),
    });
    expect(Result.isFailure(empty)).toBe(false);
    if (Result.isFailure(empty)) return;
    expect(empty.success).toEqual({ kind: "not_yet", retryAfter: 2 });

    fake.fail.list = true;
    const indeterminate = await coordinator.enroll({
      token: tokens[0] ?? "",
      identity: identity(31),
    });
    expect(Result.isFailure(indeterminate)).toBe(true);
    if (!Result.isFailure(indeterminate)) return;
    expect(indeterminate.failure._tag).toBe("PloyzProviderError");
  });

  it("retains the committed assignment after publication failure and conflicts on changed retry inputs", async () => {
    const fake = fakeRelay(harness.database);
    const founder = await fake.coordinator.enroll({ token: (tokens[0] ?? ""), identity: identity(0) });
    if (Result.isFailure(founder) || founder.success.kind !== "initialize") throw new Error("Founder missing");
    fake.held.push({ machineId: heldMachineId });
    await fake.coordinator.completeFounding({ token: (tokens[0] ?? ""), machineId: heldMachineId, pairingCredential: founder.success.pairing.secret });
    fake.fail.publish = true;
    const failed = await fake.coordinator.enroll({ token: (tokens[1] ?? ""), identity: identity(1) });
    expect(failed).toMatchObject({ success: { kind: "not_yet" } });
    const saved = await harness.pool.query("select assignments from enrollment_allocation");
    expect(saved.rows[0].assignments).toEqual(fake.published);
    fake.fail.publish = false;
    // A fresh coordinator has no worker-local retry state.
    const fresh = fakeRelay(harness.database);
    fresh.held.push({ machineId: heldMachineId });
    const resumed = await fresh.coordinator.enroll({ token: (tokens[0] ?? ""), identity: identity(1) });
    expect(resumed).toMatchObject({ success: { kind: "join" } });
    expect(fresh.published).toEqual(fake.published);
    const changed = await fresh.coordinator.enroll({ token: (tokens[0] ?? ""), identity: { ...identity(1), requestedStorage: "zfs" } });
    expect(changed).toMatchObject({ failure: { _tag: "Conflict" } });
    expect(fresh.published).toHaveLength(1);
  });

  it("returns a permanent publication conflict without trying a stale Entry or freeing the assignment", async () => {
    const fake = fakeRelay(harness.database);
    const founder = await fake.coordinator.enroll({ token: tokens[0] ?? "", identity: identity(0) });
    if (Result.isFailure(founder) || founder.success.kind !== "initialize") throw new Error("Founder missing");
    fake.held.push({ machineId: heldMachineId });
    await fake.coordinator.completeFounding({ token: tokens[0] ?? "", machineId: heldMachineId, pairingCredential: founder.success.pairing.secret });
    fake.held.push({ machineId: identity(0).machineId });
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

  it("releases transaction locks before publication and falls back to another held Entry", async () => {
    const tried: string[] = [];
    const assignment = await harness.runEffect(registerThroughHeldList({
      organizationId, relayUrl: "https://relay.example.test", bearer: "dial", pairing: "cluster-a",
      held: [heldMachine, identity(0).machineId], identity: registerRequestFromEnrollmentIdentity(identity(1)),
    }).pipe(Effect.provide(makePloyzLayer({
      connect: async () => { throw new Error("unused"); },
      observeEnrollment: async () => snapshot,
      publishEnrollment: async (_url, _bearer, pairing, entry, assignment) => {
        tried.push(entry);
        // Independent transaction must proceed while the network call is in flight.
        const next = await harness.runEffect(reserveEnrollmentAssignment({
          organizationId, pairing, identity: registerRequestFromEnrollmentIdentity(identity(2)), snapshot,
        }));
        expect(next.machine.subnet).not.toBe(assignment.machine.subnet);
        if (entry === heldMachine) throw new Error("Entry unreachable");
        return { assigned_machine: assignment.machine, visible_peers: [], target_versions: {} };
      },
    }))));
    expect(assignment.kind).toBe("registered");
    expect(tried).toEqual([heldMachine, identity(0).machineId]);
  });

  it("accepts both exact completions when concurrent callbacks race the ready CAS", async () => {
    let releaseLists!: () => void;
    let reportBothListed!: () => void;
    const allowLists = new Promise<void>((resolve) => {
      releaseLists = resolve;
    });
    const bothListed = new Promise<void>((resolve) => {
      reportBothListed = resolve;
    });
    let listCalls = 0;
    const relay: EnrollmentRelayService = {
      inspectHolding: () =>
        Effect.promise(async () => {
        listCalls += 1;
        if (listCalls === 2) reportBothListed();
        await allowLists;
          return { kind: "held" as const, held: [heldMachine] };
        }),
      registerAvailable: () =>
        Effect.succeed({ kind: "registered" as const, registration }),
      revokeIfEmpty: () => Effect.succeed("held" as const),
      revokePairing: () => Effect.void,
    };
    const coordinator = enrollmentTestClient(
      harness.database,
      relay,
    );
    const pending = await coordinator.enroll({
      token: tokens[0] ?? "",
      identity: identity(0),
    });
    expect(Result.isFailure(pending)).toBe(false);
    if (Result.isFailure(pending) || pending.success.kind !== "initialize") return;
    const pairingCredential = pending.success.pairing.secret;

    const callback = () =>
      coordinator.completeFounding({
        token: tokens[0] ?? "",
        machineId: heldMachineId,
        pairingCredential,
      });
    const completions = Promise.all([callback(), callback()]);
    await bothListed;
    releaseLists();
    const outcomes = await completions;

    expect(outcomes.every((outcome) => !Result.isFailure(outcome))).toBe(true);
  });

  it("abandons an empty pending attempt without invalidating enrollment tokens", async () => {
    const fake = fakeRelay(harness.database);
    const coordinator = fake.coordinator;
    const pending = await coordinator.enroll({
      token: tokens[0] ?? "",
      identity: identity(0),
    });
    expect(Result.isFailure(pending)).toBe(false);
    if (Result.isFailure(pending) || pending.success.kind !== "initialize") return;

    const reset = await coordinator.resetPendingEnrollment(organizationId);

    expect(Result.isFailure(reset)).toBe(false);
    expect(fake.revokedPairings).toEqual([pending.success.pairing.secret]);
    const persisted = await harness.pool.query<{
      pairing_count: number;
      token_count: number;
    }>(`
      select
        (select count(*)::int from organization_pairing where organization_id = '${organizationId}') as pairing_count,
        (select count(*)::int from machine_enrollment_token where organization_id = '${organizationId}') as token_count
    `);
    expect(persisted.rows[0]).toEqual({ pairing_count: 0, token_count: 2 });
  });

  it("rejects a concurrent completion once reset owns the pending generation", async () => {
    let releaseResetList!: () => void;
    let reportResetListStarted!: () => void;
    let reportCompletionListStarted!: () => void;
    const resetListStarted = new Promise<void>((resolve) => {
      reportResetListStarted = resolve;
    });
    const completionListStarted = new Promise<void>((resolve) => {
      reportCompletionListStarted = resolve;
    });
    const allowResetList = new Promise<void>((resolve) => {
      releaseResetList = resolve;
    });
    let listCalls = 0;
    const relay: EnrollmentRelayService = {
      inspectHolding: () =>
        Effect.sync(() => {
          listCalls += 1;
          reportCompletionListStarted();
          return { kind: "held" as const, held: [heldMachine] };
        }),
      registerAvailable: () =>
        Effect.succeed({ kind: "registered" as const, registration }),
      revokeIfEmpty: () =>
        Effect.promise(async () => {
          listCalls += 1;
          reportResetListStarted();
          await allowResetList;
          return "revoked" as const;
        }),
      revokePairing: () => Effect.void,
    };
    const coordinator = enrollmentTestClient(
      harness.database,
      relay,
    );
    const pending = await coordinator.enroll({
      token: tokens[0] ?? "",
      identity: identity(0),
    });
    expect(Result.isFailure(pending)).toBe(false);
    if (Result.isFailure(pending) || pending.success.kind !== "initialize") return;

    const resetPromise = coordinator.resetPendingEnrollment(organizationId);
    await resetListStarted;
    const completionPromise = coordinator.completeFounding({
      token: tokens[0] ?? "",
      machineId: heldMachineId,
      pairingCredential: pending.success.pairing.secret,
    });
    await completionListStarted;
    releaseResetList();
    const [reset, completion] = await Promise.all([
      resetPromise,
      completionPromise,
    ]);

    expect(Result.isFailure(reset)).toBe(false);
    expect(Result.isFailure(completion)).toBe(true);
    if (!Result.isFailure(completion)) return;
    expect(completion.failure._tag).toBe("Conflict");
    const pairing = await harness.pool.query(
      `select 1 from organization_pairing where organization_id = '${organizationId}'`,
    );
    expect(pairing.rowCount).toBe(0);
  });

  it("refuses reset when Relay still holds the pending founder", async () => {
    const fake = fakeRelay(harness.database);
    const coordinator = fake.coordinator;
    const pending = await coordinator.enroll({
      token: tokens[0] ?? "",
      identity: identity(0),
    });
    expect(Result.isFailure(pending)).toBe(false);
    if (Result.isFailure(pending) || pending.success.kind !== "initialize") return;
    fake.held.push({ machineId: heldMachineId });

    const reset = await coordinator.resetPendingEnrollment(organizationId);

    expect(Result.isFailure(reset)).toBe(true);
    if (!Result.isFailure(reset)) return;
    expect(reset.failure._tag).toBe("Conflict");
    expect(fake.revokedPairings).toEqual([]);
    const pairing = await harness.pool.query(
      `select founder_machine_id from organization_pairing where organization_id = '${organizationId}'`,
    );
    expect(pairing.rows[0]?.founder_machine_id).toBeNull();

    const completed = await coordinator.completeFounding({
      token: tokens[0] ?? "",
      machineId: heldMachineId,
      pairingCredential: pending.success.pairing.secret,
    });
    expect(Result.isFailure(completed)).toBe(false);
  });

  it("refuses reset when Relay evidence is indeterminate", async () => {
    const fake = fakeRelay(harness.database);
    const coordinator = fake.coordinator;
    await coordinator.enroll({
      token: tokens[0] ?? "",
      identity: identity(0),
    });
    fake.fail.list = true;

    const reset = await coordinator.resetPendingEnrollment(organizationId);

    expect(Result.isFailure(reset)).toBe(true);
    if (!Result.isFailure(reset)) return;
    expect(reset.failure._tag).toBe("PloyzProviderError");
    expect(fake.revokedPairings).toEqual([]);
    const pairing = await harness.pool.query(
      `select 1 from organization_pairing where organization_id = '${organizationId}'`,
    );
    expect(pairing.rowCount).toBe(1);
  });

  it("keeps the pending attempt when Relay cannot revoke its credential", async () => {
    const fake = fakeRelay(harness.database);
    const coordinator = fake.coordinator;
    await coordinator.enroll({
      token: tokens[0] ?? "",
      identity: identity(0),
    });
    fake.fail.revoke = true;

    const reset = await coordinator.resetPendingEnrollment(organizationId);

    expect(Result.isFailure(reset)).toBe(true);
    if (!Result.isFailure(reset)) return;
    expect(reset.failure._tag).toBe("PloyzProviderError");
    const pairing = await harness.pool.query(
      `select 1 from organization_pairing where organization_id = '${organizationId}'`,
    );
    expect(pairing.rowCount).toBe(1);
  });

  it("refuses reset when Relay returns a nonempty list without Machine evidence", async () => {
    const fake = fakeRelay(harness.database);
    const coordinator = fake.coordinator;
    await coordinator.enroll({
      token: tokens[0] ?? "",
      identity: identity(0),
    });
    fake.held.push({ unexpected: true });

    const reset = await coordinator.resetPendingEnrollment(organizationId);

    expect(Result.isFailure(reset)).toBe(true);
    if (!Result.isFailure(reset)) return;
    expect(reset.failure._tag).toBe("PloyzProviderError");
    expect(fake.revokedPairings).toEqual([]);
    const pairing = await harness.pool.query(
      `select 1 from organization_pairing where organization_id = '${organizationId}'`,
    );
    expect(pairing.rowCount).toBe(1);
  });

  it("refuses reset after the Organization is ready", async () => {
    const fake = fakeRelay(harness.database);
    const coordinator = fake.coordinator;
    const pending = await coordinator.enroll({
      token: tokens[0] ?? "",
      identity: identity(0),
    });
    expect(Result.isFailure(pending)).toBe(false);
    if (Result.isFailure(pending) || pending.success.kind !== "initialize") return;
    fake.held.push({ machineId: heldMachineId });
    await coordinator.completeFounding({
      token: tokens[0] ?? "",
      machineId: heldMachineId,
      pairingCredential: pending.success.pairing.secret,
    });
    const listCallsBeforeReset = fake.listCalls();

    const reset = await coordinator.resetPendingEnrollment(organizationId);

    expect(Result.isFailure(reset)).toBe(true);
    if (!Result.isFailure(reset)) return;
    expect(reset.failure._tag).toBe("Conflict");
    expect(fake.listCalls()).toBe(listCallsBeforeReset);
    const pairing = await harness.pool.query<{
      founder_machine_id: string | null;
    }>(
      `select founder_machine_id from organization_pairing where organization_id = '${organizationId}'`,
    );
    expect(pairing.rows[0]?.founder_machine_id).toBe(heldMachineId);
  });

  it("refuses reset when no pending attempt exists", async () => {
    const fake = fakeRelay(harness.database);
    const coordinator = fake.coordinator;

    const reset = await coordinator.resetPendingEnrollment(organizationId);

    expect(Result.isFailure(reset)).toBe(true);
    if (!Result.isFailure(reset)) return;
    expect(reset.failure._tag).toBe("Conflict");
    expect(fake.listCalls()).toBe(0);
  });

  it("lets one waiting Machine claim a new generation after reset", async () => {
    const fake = fakeRelay(harness.database);
    const coordinator = fake.coordinator;
    const abandoned = await coordinator.enroll({
      token: tokens[0] ?? "",
      identity: identity(0),
    });
    expect(Result.isFailure(abandoned)).toBe(false);
    if (Result.isFailure(abandoned) || abandoned.success.kind !== "initialize")
      return;
    await coordinator.resetPendingEnrollment(organizationId);

    const retriedWaiters = await Promise.all(
      Array.from({ length: 20 }, (_, index) =>
        coordinator.enroll({
          token: tokens[index % tokens.length] ?? "",
          identity: identity(index + 1),
        }),
      ),
    );
    const directives = retriedWaiters.flatMap((result) =>
      Result.isFailure(result) ? [] : [result.success],
    );

    expect(directives).toHaveLength(20);
    const founders = directives.filter(
      (directive) => directive.kind === "initialize",
    );
    expect(founders).toHaveLength(1);
    expect(
      directives.filter((directive) => directive.kind === "not_yet"),
    ).toHaveLength(19);
    const founder = founders[0];
    if (founder?.kind !== "initialize") return;
    expect(founder.resumed).toBe(false);
    expect(founder.pairing.secret).not.toBe(abandoned.success.pairing.secret);
  });

  it("rejects completion from an abandoned Pairing generation", async () => {
    const fake = fakeRelay(harness.database);
    const coordinator = fake.coordinator;
    const abandoned = await coordinator.enroll({
      token: tokens[0] ?? "",
      identity: identity(0),
    });
    expect(Result.isFailure(abandoned)).toBe(false);
    if (Result.isFailure(abandoned) || abandoned.success.kind !== "initialize")
      return;

    const reset = await coordinator.resetPendingEnrollment(organizationId);
    expect(Result.isFailure(reset)).toBe(false);
    const replacement = await coordinator.enroll({
      token: tokens[1] ?? "",
      identity: identity(1),
    });
    expect(Result.isFailure(replacement)).toBe(false);
    if (Result.isFailure(replacement) || replacement.success.kind !== "initialize")
      return;
    expect(replacement.success.pairing.secret).not.toBe(
      abandoned.success.pairing.secret,
    );

    fake.held.push({ machineId: heldMachineId });
    const stale = await coordinator.completeFounding({
      token: tokens[0] ?? "",
      machineId: heldMachineId,
      pairingCredential: abandoned.success.pairing.secret,
    });
    expect(Result.isFailure(stale)).toBe(true);
    if (!Result.isFailure(stale)) return;
    expect(stale.failure._tag).toBe("Conflict");
  });

  it("keeps pairing decrypt failures in the typed Effect channel", async () => {
    const stale = makeSecretEncryption("stale-app-encryption-secret-1234567890");
    await harness.pool.query(
      `insert into organization_pairing (
        organization_id, encrypted_pairing_secret, founder_public_key
      ) values ($1, $2::jsonb, $3)`,
      [
        organizationId,
        JSON.stringify(stale.encrypt("ppair_stale")),
        identity(0).publicKey,
      ],
    );

    const fake = fakeRelay(harness.database);
    const enrolled = await fake.coordinator.enroll({
      token: tokens[0] ?? "",
      identity: identity(0),
    });
    expect(Result.isFailure(enrolled)).toBe(true);
    if (!Result.isFailure(enrolled)) return;
    expect(enrolled.failure._tag).toBe("PairingSecretDecryptFailure");

    const revoked = await fake.coordinator.tryRevokePairing(organizationId);
    expect(revoked).toBe(false);
    expect(fake.revokedPairings).toEqual([]);
  });

  it("rejects invalid pending and ready Organization Pairing shapes", async () => {
    await expect(
      harness.pool.query(`
        insert into organization_pairing (
          organization_id, encrypted_pairing_secret
        ) values ('${organizationId}', '{}'::jsonb)
      `),
    ).rejects.toMatchObject({ code: "23514" });

    await expect(
      harness.pool.query(`
        insert into organization_pairing (
          organization_id, encrypted_pairing_secret, founder_machine_id
        ) values (
          '${organizationId}', '{}'::jsonb,
          'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
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
          founder_public_key, founder_machine_id
        ) values (
          '${organizationId}', '{}'::jsonb, 'founder', 'not-a-machine'
        )
      `),
    ).rejects.toMatchObject({ code: "23514" });
  });
});
