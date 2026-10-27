import { createHash } from "node:crypto";
import { it } from "@effect/vitest";
import type { Act, CloudQuery, LogsAnswer, MachineId, MachineName, MachineUpgradeAttemptId, RuntimeWatchView, ServerRef, ServiceVerb, VolumeId, VolumeRun } from "@ployz/sdk";
import { eq, sql } from "drizzle-orm";
import { Effect, Schema } from "effect";
import { Inngest } from "inngest";
import { describe, expect, vi } from "vitest";
import { asRecord } from "#/lib/json";
import { asTestDouble } from "#/lib/test-double";
import { callCloud, NO_SERVERS_MESSAGE } from "#/modules/agent/call-cloud.server";
import { decideApproval, operationDigest } from "#/modules/approvals/approvals.server";
import type { Caller } from "#/modules/identity/actor";
import { presetGrant } from "#/modules/identity/token-grant";
import { freshApproval, freshOperationDigest, UNREACHABLE_MESSAGE } from "#/modules/machines/server-operations.server";
import { dropRemovedServer, forgetEmptiedPairing } from "#/modules/machines/pairing-removal.server";
import { requestMachineRemoveAttempt } from "#/modules/machines/machine-removal.repository";
import { enqueueMachineRemove } from "#/modules/machines/machine-removal.server";
import { requestCliServerDrain } from "#/modules/machines/server-drain.server";
import { forgetServerFromDashboard } from "#/modules/operations/operations.server";
import { machineEnrollmentToken, machineRemoveAttempt, organizationMachine, serverDrainAttempt } from "#/modules/machines/tables";
import { attemptIdOf } from "#/modules/operations/keyed.server";
import { operationsOn } from "#/modules/operations/operation-history.server";
import { operationRequest } from "#/modules/operations/tables";
import { serviceAct } from "#/modules/service-act/tables";
import { operationApprovals } from "#/modules/approvals/tables";
import { recordUpgradeAttempt } from "#/modules/server-upgrade/server-upgrade.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { OrganizationRuntime, type ScopedRuntimeClientSession } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { sealedSecret } from "#/test/sealed-secret";
import { acmeWeb, enrollStoreServer, seedStoreGitService, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000c101";
const DRAIN = "00000000-0000-4000-8000-00000000c102";
const OTHER_DRAIN = "00000000-0000-4000-8000-00000000c103";
const APPROVAL = "00000000-0000-4000-8000-00000000c10a";
const UPGRADE = "b".repeat(32);
const PROJECT = "00000000-0000-4000-8000-00000000c104";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000c105";
const SERVICE = "00000000-0000-4000-8000-00000000c106";
const WEB = { environment: ENVIRONMENT, service: SERVICE };
const DEPLOYMENT = "00000000-0000-4000-8000-00000000c107";
const MACHINE = "a".repeat(32) as MachineId;
const OTHER_MACHINE = "e".repeat(32) as MachineId;
const VOLUME = "00000000-0000-4000-8000-00000000c108" as VolumeId;
const Asked = Schema.Struct({ approval: Schema.String, approval_id: Schema.String });

describe("callCloud", () => {
  it.live("answers an operations query as OperationsAnswer, the shape the agent tools decode", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      const { drizzle } = yield* Database.pipe(Effect.provide(services));
      yield* drizzle.insert(serverDrainAttempt).values({ id: DRAIN, organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId });
      const caller: Caller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "session-1" }, role: "owner", support: null, via: "agent" };

      const heard = yield* callCloud(caller, { ask: { query: "operations", subject: { kind: "server", machine: MACHINE }, attempt: null, limit: 5 } })
        .pipe(Effect.provide(services));

      expect(heard).toEqual({
        ok: true,
        value: { operations: [expect.objectContaining({ attempt: DRAIN, run: `drain ${MACHINE}`, state: "accepted" })] },
      });
    }));

  it.live("names a Server run by the name its act resolved when the runtime has no frame", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      const { drizzle } = yield* Database.pipe(Effect.provide(services));
      yield* drizzle.insert(serverDrainAttempt).values({ id: DRAIN, organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId });

      const [run] = yield* operationsOn(ORGANIZATION, { kind: "server", machine: MACHINE }, DRAIN, 1, "machine-1").pipe(Effect.provide(services));

      expect(run).toEqual(expect.objectContaining({ attempt: DRAIN, run: "drain machine-1" }));
    }));

  it.live("says why a Server's Drain ended without a report", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      const { drizzle } = yield* Database.pipe(Effect.provide(services));
      const endedAt = new Date();
      yield* drizzle.insert(serverDrainAttempt).values([
        { id: DRAIN, organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId, state: "failed", endCode: "not_started", endedAt },
        {
          id: OTHER_DRAIN, organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId,
          state: "failed", endCode: "refused", refusalMessage: "Drains need a newer Ployz on machine-1.", endedAt,
        },
      ]);
      const read = (attempt: string) =>
        operationsOn(ORGANIZATION, { kind: "server", machine: MACHINE }, attempt, 1, "machine-1").pipe(Effect.provide(services));

      expect(yield* read(DRAIN)).toEqual([expect.objectContaining({ state: "failed", reason: "The drain never started." })]);
      expect(yield* read(OTHER_DRAIN)).toEqual([expect.objectContaining({ state: "failed", reason: "Drains need a newer Ployz on machine-1." })]);
    }));

  // Catches a lost Drain and an interrupted one reaching the agent tools as the same `unknown`, told apart only by words.
  it.live("names how a Drain with no verdict ended", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      const { drizzle } = yield* Database.pipe(Effect.provide(services));
      const ran = (inngestRunId: string) => ({ state: "unknown", inngestRunId, startedAt: new Date(), endedAt: new Date() }) as const;
      yield* drizzle.insert(serverDrainAttempt).values([
        { id: DRAIN, organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId, ...ran("run-1"), endCode: "lost" },
        { id: OTHER_DRAIN, organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId, ...ran("run-2"), endCode: "interrupted" },
      ]);
      const read = (attempt: string) =>
        operationsOn(ORGANIZATION, { kind: "server", machine: MACHINE }, attempt, 1, "machine-1").pipe(Effect.provide(services));

      expect(yield* read(DRAIN)).toEqual([expect.objectContaining({ state: "unknown", end: "lost" })]);
      expect(yield* read(OTHER_DRAIN)).toEqual([expect.objectContaining({ state: "unknown", end: "interrupted" })]);
    }));

  const LOGS: CloudQuery = {
    query: "logs",
    namespace: "shop-production",
    service: "web",
    deployment: null,
    server: null,
    lines: 50,
    since: { secs: 600, nanos: 0 },
  };
  const TAIL: LogsAnswer = {
    servers: [{ server: "server-1", text: { status: "shown", lines: ["listening on :8080\n", "token [secret TOKEN]\n"], cut: false, redacted: 1 } }],
    seen: { seen_by: null, observed_at: 1, read_at: 1, age_seconds: 0, from: "server", unanswered: [] },
    notes: [],
  };
  const member: Caller = { userId: "user-1", organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "session-1" }, role: "owner", support: null, via: "agent" };

  const runtime = (opened: ScopedRuntimeClientSession, opens: Array<string>) => Effect.provideService(OrganizationRuntime, {
    cancel: () => Effect.void,
    open: (organizationId: string) => Effect.sync(() => {
      opens.push(organizationId);
      return opened;
    }),
  });

  // Catches a polled Server run named by its raw id whenever the runtime has no frame to name it.
  it.live("names a polled Server run by the name Cloud keeps when the runtime has no frame", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      yield* enrollStoreServer(ORGANIZATION, { secret: "ppair_fixture_store", server: MACHINE }).pipe(Effect.provide(services));
      const { drizzle } = yield* Database.pipe(Effect.provide(services));
      yield* drizzle.update(organizationMachine).set({ machineName: "machine-1" as MachineName });
      yield* drizzle.insert(serverDrainAttempt).values({ id: DRAIN, organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId });
      yield* drizzle.insert(machineRemoveAttempt).values({ organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId, confirmDataLoss: [] });
      const caller: Caller = { ...member, userId };

      const heard = yield* callCloud(caller, { ask: { query: "operations", subject: { kind: "server", machine: MACHINE }, attempt: null, limit: 5 } })
        .pipe(runtime({ status: "unreachable", error: null }, []), Effect.provide(services));

      expect(heard).toEqual({
        ok: true,
        value: { operations: expect.arrayContaining([
          expect.objectContaining({ attempt: DRAIN, run: "drain machine-1" }),
          expect.objectContaining({ run: "remove machine-1" }),
        ]) },
      });
    }));

  // Catches a failed Drain missing from the answer once later runs fill the limit the agent asked for.
  it.live("answers a Server's operations with its newest Drain with a verdict, past the limit", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      yield* enrollStoreServer(ORGANIZATION, { secret: "ppair_fixture_store", server: MACHINE }).pipe(Effect.provide(services));
      const { drizzle } = yield* Database.pipe(Effect.provide(services));
      yield* drizzle.update(organizationMachine).set({ machineName: "machine-1" as MachineName });
      const longAgo = new Date(Date.now() - 60_000);
      yield* drizzle.insert(serverDrainAttempt).values({
        id: DRAIN, organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId, state: "failed", endCode: "not_started", requestedAt: longAgo, endedAt: longAgo,
      });
      yield* drizzle.insert(machineRemoveAttempt).values({ organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId, confirmDataLoss: [] });
      const caller: Caller = { ...member, userId };

      const heard = yield* callCloud(caller, { ask: { query: "operations", subject: { kind: "server", machine: MACHINE }, attempt: null, limit: 1 } })
        .pipe(runtime({ status: "unreachable", error: null }, []), Effect.provide(services));

      expect(heard).toEqual({
        ok: true,
        value: {
          operations: [expect.objectContaining({ run: "remove machine-1" })],
          settled_drain: expect.objectContaining({ attempt: DRAIN, run: "drain machine-1", state: "failed" }),
        },
      });
    }));

  // Catches an act's reply naming its Server by a name Cloud kept before a rename reached it.
  it.live("names an act's Server by the name the act resolved, over a stale one Cloud kept", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      yield* enrollStoreServer(ORGANIZATION, { secret: "ppair_fixture_store", server: MACHINE }).pipe(Effect.provide(services));
      const { drizzle } = yield* Database.pipe(Effect.provide(services));
      yield* drizzle.update(organizationMachine).set({ machineName: "old-name" as MachineName });
      // Asked for from this session, for exactly this forget, and already spent on the removal below.
      yield* drizzle.insert(operationApprovals).values({
        id: APPROVAL, organizationId: ORGANIZATION, subject: `server:${MACHINE}`, requestedByUserId: userId, via: "agent", credentialKind: "session", credentialId: "session-1",
        command: "forget", digest: `forget:server:${MACHINE}`, status: "approved", decidedByUserId: userId, decidedAt: new Date(), usedAt: new Date(),
        review: asTestDouble<typeof operationApprovals.$inferInsert.review>()({ effects: [], operation: { verb: "forget", name: MACHINE, preview: null } }),
      });
      const [removal] = yield* drizzle.insert(machineRemoveAttempt).values({
        organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId, confirmDataLoss: [], noReset: true, approvalId: APPROVAL,
      }).returning();
      const forget: Act = { attempt: "c".repeat(32), digest: "d".repeat(64), action: { action: "forget_server", server: { machine: MACHINE, name: "machine-1" } } };
      const unreachable = runtime({ status: "unreachable", error: null }, []);

      const acted = yield* callCloud({ ...member, userId }, { act: forget }, APPROVAL).pipe(unreachable, Effect.provide(services));
      const polled = yield* callCloud({ ...member, userId }, { ask: { query: "operations", subject: { kind: "server", machine: MACHINE }, attempt: null, limit: 5 } })
        .pipe(unreachable, Effect.provide(services));

      expect(acted).toEqual({ ok: true, value: expect.objectContaining({ attempt: removal?.id, run: "remove machine-1" }) });
      // A poll carries no name of its own, so the one Cloud kept is the best it has.
      expect(polled).toEqual({ ok: true, value: { operations: [expect.objectContaining({ attempt: removal?.id, run: "remove old-name" })] } });
    }));

  // Catches a removed Server's runs named by its raw id once Cloud drops its row.
  it.live("names a removed Server's runs by the name it enrolled with while another Server stays", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      yield* enrollStoreServer(ORGANIZATION, { secret: "ppair_fixture_store", server: MACHINE }).pipe(Effect.provide(services));
      const generation = createHash("sha256").update("ppair_fixture_store").digest("hex");
      const { drizzle } = yield* Database.pipe(Effect.provide(services));
      const encryption = yield* SecretEncryption.pipe(Effect.provide(services));
      yield* drizzle.update(organizationMachine).set({ machineName: "machine-1" as MachineName });
      yield* drizzle.insert(organizationMachine).values({
        organizationId: ORGANIZATION, machineId: OTHER_MACHINE, clusterKey: generation,
        encryptedCapability: encryption.encrypt(`ployz1:cloud:${OTHER_MACHINE}`), machineName: "machine-2" as MachineName,
      });
      yield* drizzle.insert(machineEnrollmentToken).values({
        organizationId: ORGANIZATION, tokenHash: "f".repeat(64), createdByUserId: userId, expiresAt: new Date(Date.now() + 60_000),
        joinedMachineId: MACHINE, claimedMachineId: MACHINE, claimedPublicKey: "machine-key", claimedName: "machine-1" as MachineName, claimedAt: new Date(),
      });
      yield* drizzle.insert(serverDrainAttempt).values({ id: DRAIN, organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId });
      yield* drizzle.insert(machineRemoveAttempt).values({ organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId, confirmDataLoss: [] });
      expect(yield* dropRemovedServer({ organizationId: ORGANIZATION, machineId: MACHINE, generation, removal: "reset" }).pipe(Effect.provide(services)))
        .toEqual({ kind: "others_remain" });

      const heard = yield* callCloud({ ...member, userId }, { ask: { query: "operations", subject: { kind: "server", machine: MACHINE }, attempt: null, limit: 5 } })
        .pipe(runtime({ status: "unreachable", error: null }, []), Effect.provide(services));

      expect(heard).toEqual({
        ok: true,
        value: { operations: expect.arrayContaining([
          expect.objectContaining({ attempt: DRAIN, run: "drain machine-1" }),
          expect.objectContaining({ run: "remove machine-1" }),
        ]) },
      });
    }));

  /** Starts a drain, a removal and an upgrade of `MACHINE` the way Cloud starts them, each from a caller that showed `serverName`. */
  const startServerRuns = (userId: string, serverName: MachineName) => Effect.gen(function* () {
    yield* requestCliServerDrain({ organizationId: ORGANIZATION, userId }, {
      attemptId: DRAIN, machineId: MACHINE, targets: [], approvalId: null, planDigest: "p", serverName,
    });
    yield* requestMachineRemoveAttempt({ organizationId: ORGANIZATION, requestedByUserId: userId, machineId: MACHINE, confirmDataLoss: [], serverName });
    yield* recordUpgradeAttempt({
      request: { organizationId: ORGANIZATION, machineId: MACHINE, trigger: "manual", userId },
      attemptId: UPGRADE as MachineUpgradeAttemptId, channel: "stable", fromVersion: "", inngestRunId: "run-1", serverName,
    });
  });
  const serverRuns = (name: string) => ({
    ok: true,
    value: { operations: expect.arrayContaining([
      expect.objectContaining({ attempt: DRAIN, run: `drain ${name}` }),
      expect.objectContaining({ run: `remove ${name}` }),
      expect.objectContaining({ run: `upgrade ${name}` }),
    ]) },
  });

  // Catches a renamed Server's runs named by the name it enrolled with once Cloud drops its row.
  it.live("names a renamed then removed Server's runs by the name it had when they started", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      yield* enrollStoreServer(ORGANIZATION, { secret: "ppair_fixture_store", server: MACHINE }).pipe(Effect.provide(services));
      const generation = createHash("sha256").update("ppair_fixture_store").digest("hex");
      const { drizzle } = yield* Database.pipe(Effect.provide(services));
      const encryption = yield* SecretEncryption.pipe(Effect.provide(services));
      yield* drizzle.insert(organizationMachine).values({
        organizationId: ORGANIZATION, machineId: OTHER_MACHINE, clusterKey: generation,
        encryptedCapability: encryption.encrypt(`ployz1:cloud:${OTHER_MACHINE}`), machineName: "machine-2" as MachineName,
      });
      yield* drizzle.insert(machineEnrollmentToken).values({
        organizationId: ORGANIZATION, tokenHash: "f".repeat(64), createdByUserId: userId, expiresAt: new Date(Date.now() + 60_000),
        joinedMachineId: MACHINE, claimedMachineId: MACHINE, claimedPublicKey: "machine-key", claimedName: "machine-1" as MachineName, claimedAt: new Date(),
      });
      yield* drizzle.update(organizationMachine).set({ machineName: "web-1" as MachineName }).where(sql`${organizationMachine.machineId} = ${MACHINE}`);
      yield* startServerRuns(userId, "web-1").pipe(Effect.provide(services));
      expect(yield* dropRemovedServer({ organizationId: ORGANIZATION, machineId: MACHINE, generation, removal: "reset" }).pipe(Effect.provide(services)))
        .toEqual({ kind: "others_remain" });

      const heard = yield* callCloud({ ...member, userId }, { ask: { query: "operations", subject: { kind: "server", machine: MACHINE }, attempt: null, limit: 5 } })
        .pipe(runtime({ status: "unreachable", error: null }, []), Effect.provide(services));

      expect(heard).toEqual(serverRuns("web-1"));
    }));

  // Catches the last Server's runs named by its raw id once Cloud forgets the pairing and every name it kept.
  it.live("names the last removed Server's runs by the name it had when they started", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      yield* enrollStoreServer(ORGANIZATION, { secret: "ppair_fixture_store", server: MACHINE }).pipe(Effect.provide(services));
      const generation = createHash("sha256").update("ppair_fixture_store").digest("hex");
      const { drizzle } = yield* Database.pipe(Effect.provide(services));
      yield* drizzle.update(organizationMachine).set({ machineName: "machine-1" as MachineName });
      yield* startServerRuns(userId, "machine-1").pipe(Effect.provide(services));
      expect(yield* dropRemovedServer({ organizationId: ORGANIZATION, machineId: MACHINE, generation, removal: "reset" }).pipe(Effect.provide(services)))
        .toEqual({ kind: "last" });
      expect(yield* forgetEmptiedPairing(ORGANIZATION, generation).pipe(Effect.provide(services))).toEqual({ kind: "released" });

      const heard = yield* callCloud({ ...member, userId }, { ask: { query: "operations", subject: { kind: "server", machine: MACHINE }, attempt: null, limit: 5 } })
        .pipe(runtime({ status: "unreachable", error: null }, []), Effect.provide(services));

      expect(heard).toEqual(serverRuns("machine-1"));
    }));

  /** A Cluster whose frame names `MACHINE` `name`, online and idle, and whose removal loses nothing. */
  const framed = (name: string): ScopedRuntimeClientSession => ({
    status: "connected",
    connected: asTestDouble<PloyzSession>()({
      watchFirstFrame: () => Effect.succeed(asTestDouble<RuntimeWatchView>()({
        machines: [{ membership: "up", machine: { id: MACHINE, name, accepts_ingress: true, public_ip: "203.0.113.2", runtime: { running_builds: 0, daemon_version: "0.1.0" } } }],
        services: [],
        volumes: [],
      })),
      dataLossIfMachineRemoved: () => Effect.succeed({ data_loss: [] }),
    }),
  });

  const freshRuns = [
    ["drain", { action: "drain", server: { machine: MACHINE, name: "new" } }],
    ["upgrade", { action: "upgrade", server: { machine: MACHINE, name: "new" } }],
    ["remove", { action: "remove_server", server: { machine: MACHINE, name: "new" }, loss: null }],
  ] as const satisfies ReadonlyArray<readonly [string, Act["action"]]>;
  for (const [verb, action] of freshRuns) {
    // Catches a fresh run stored under the stale name Cloud kept, so its reply and its poll name the Server's old name.
    it.live(`names a fresh ${verb} by the name its act resolved when Cloud kept a stale one`, () =>
      Effect.gen(function* () {
        const inngest = new Inngest({ id: "call-cloud-test" });
        vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
        const services = yield* storeTestCloud({ inngest });
        const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
        yield* enrollStoreServer(ORGANIZATION, { secret: "ppair_fixture_store", server: MACHINE }).pipe(Effect.provide(services));
        const { drizzle } = yield* Database.pipe(Effect.provide(services));
        yield* drizzle.update(organizationMachine).set({ machineName: "old" as MachineName });
        const caller: Caller = { ...member, userId };

        const act = (approvalId: string | null) => callCloud(caller, { act: { attempt: "c".repeat(32), digest: "d".repeat(64), action } }, approvalId)
          .pipe(runtime(framed("new"), []), Effect.provide(services));
        const asked = yield* act(null);
        // A removal waits on a human; approving it lets the same act start the run.
        const started = asked.ok || asked.refusal.code !== "approval_required" ? asked : yield* Effect.gen(function* () {
          const { approval, approval_id } = Schema.decodeUnknownSync(Asked)(asked.refusal.details);
          yield* decideApproval({ ...caller, via: "dashboard" }, approval_id, { approve: { digest: approval } }).pipe(Effect.provide(services));
          return yield* act(approval_id);
        });
        const polled = yield* callCloud(caller, { ask: { query: "operations", subject: { kind: "server", machine: MACHINE }, attempt: null, limit: 5 } })
          .pipe(runtime(framed("new"), []), Effect.provide(services));

        expect(started).toEqual({ ok: true, value: expect.objectContaining({ run: `${verb} new` }) });
        expect(polled).toEqual({ ok: true, value: { operations: [expect.objectContaining({ run: `${verb} new` })] } });
      }));
  }

  const dashboardRuns = [
    ["remove", (caller: Caller) => enqueueMachineRemove(caller, { organizationSlug: "shop", machineId: MACHINE, confirmDataLoss: [] })],
    ["forget", (caller: Caller) => forgetServerFromDashboard({ ...caller, via: "dashboard" }, MACHINE, "forget-1")],
  ] as const;
  for (const [verb, start] of dashboardRuns) {
    // Catches a dashboard run stored under the name Cloud kept before a rename reached it, not the one the Server's page showed.
    it.live(`names a dashboard ${verb} by the Server's name in the frame while Cloud still keeps its old one`, () =>
      Effect.gen(function* () {
        const inngest = new Inngest({ id: "call-cloud-test" });
        vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
        const services = yield* storeTestCloud({ inngest });
        const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
        yield* enrollStoreServer(ORGANIZATION, { secret: "ppair_fixture_store", server: MACHINE }).pipe(Effect.provide(services));
        const { drizzle } = yield* Database.pipe(Effect.provide(services));
        const keep = (name: MachineName) => drizzle.update(organizationMachine).set({ machineName: name });
        const caller: Caller = { ...member, userId };
        const poll = () => callCloud(caller, { ask: { query: "operations", subject: { kind: "server", machine: MACHINE }, attempt: null, limit: 5 } })
          .pipe(runtime(framed("web-2"), []), Effect.provide(services));
        yield* keep("old");

        yield* start(caller).pipe(runtime(framed("web-2"), []), Effect.provide(services));

        expect(yield* poll()).toEqual({ ok: true, value: { operations: [expect.objectContaining({ run: "remove web-2" })] } });
        yield* keep("web-2");
        expect(yield* poll()).toEqual({ ok: true, value: { operations: [expect.objectContaining({ run: "remove web-2" })] } });
      }));
  }

  // Catches a replayed act naming its run by the Server's newer name while a poll of the same run names the one it started with.
  it.live("names a replayed upgrade by the name it started with after the Server is renamed", () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "call-cloud-test" });
      vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const services = yield* storeTestCloud({ inngest });
      const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      yield* enrollStoreServer(ORGANIZATION, { secret: "ppair_fixture_store", server: MACHINE }).pipe(Effect.provide(services));
      const { drizzle } = yield* Database.pipe(Effect.provide(services));
      const upgrade = (name: string): Act => ({ attempt: "c".repeat(32), digest: "d".repeat(64), action: { action: "upgrade", server: { machine: MACHINE, name } } });
      const caller: Caller = { ...member, userId };
      yield* drizzle.update(organizationMachine).set({ machineName: "old" as MachineName });
      const started = yield* callCloud(caller, { act: upgrade("old") }).pipe(runtime(framed("old"), []), Effect.provide(services));
      expect(started).toEqual({ ok: true, value: expect.objectContaining({ run: "upgrade old" }) });

      yield* drizzle.update(organizationMachine).set({ machineName: "new" as MachineName });
      const replayed = yield* callCloud(caller, { act: upgrade("new") }).pipe(runtime(framed("new"), []), Effect.provide(services));
      const polled = yield* callCloud(caller, { ask: { query: "operations", subject: { kind: "server", machine: MACHINE }, attempt: null, limit: 5 } })
        .pipe(runtime(framed("new"), []), Effect.provide(services));

      expect(replayed).toEqual(started);
      expect(polled).toEqual({ ok: true, value: { operations: [expect.objectContaining({ run: "upgrade old" })] } });
    }));

  // Catches a Server the frame reports with no name read as `drain ` over the name Cloud kept.
  it.live("names a polled Server run by the name Cloud keeps when the frame reports it nameless", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      yield* enrollStoreServer(ORGANIZATION, { secret: "ppair_fixture_store", server: MACHINE }).pipe(Effect.provide(services));
      const { drizzle } = yield* Database.pipe(Effect.provide(services));
      yield* drizzle.update(organizationMachine).set({ machineName: "machine-1" as MachineName });
      yield* drizzle.insert(serverDrainAttempt).values({ id: DRAIN, organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId });
      const nameless: ScopedRuntimeClientSession = {
        status: "connected",
        connected: asTestDouble<PloyzSession>()({
          watchFirstFrame: () => Effect.succeed(asTestDouble<RuntimeWatchView>()({
            machines: [{ membership: "up", machine: { id: MACHINE, name: "", accepts_ingress: true, public_ip: "203.0.113.2", runtime: { running_builds: 0 } } }],
          })),
        }),
      };

      const heard = yield* callCloud({ ...member, userId }, { ask: { query: "operations", subject: { kind: "server", machine: MACHINE }, attempt: null, limit: 5 } })
        .pipe(runtime(nameless, []), Effect.provide(services));

      expect(heard).toEqual({ ok: true, value: { operations: [expect.objectContaining({ attempt: DRAIN, run: "drain machine-1" })] } });
    }));

  const connected = (
    asked: Array<CloudQuery>, readables: Array<ReadonlyArray<string> | null> = [], sealeds: Array<ReadonlyArray<string> | null> = [],
  ): ScopedRuntimeClientSession => ({
    status: "connected",
    connected: asTestDouble<PloyzSession>()({
      agentAsk: (query: CloudQuery, readable: ReadonlyArray<string> | null, sealed: ReadonlyArray<string> | null) => Effect.sync(() => {
        asked.push(query);
        readables.push(readable);
        sealeds.push(sealed);
        return TAIL;
      }),
    }),
  });

  const observing = (otherServesWeb: boolean): ScopedRuntimeClientSession => ({
    status: "connected",
    connected: asTestDouble<PloyzSession>()({
      watchFirstFrame: () => Effect.succeed(asTestDouble<RuntimeWatchView>()({
        machines: [
          { membership: "up", machine: { id: MACHINE, name: "web-2", accepts_ingress: true, public_ip: "203.0.113.2", runtime: { running_builds: 0 } } },
          { membership: "up", machine: { id: OTHER_MACHINE, name: "web-3", accepts_ingress: otherServesWeb, public_ip: "203.0.113.3", runtime: { running_builds: 0 } } },
        ],
        certificates: [{ hostname: "shop.example.com" }],
      })),
    }),
  });

  // Catches the logs ask still refused, or its lines and since dropped on the way to the runtime.
  it.live("answers a logs ask with the tail the Organization runtime reads", () =>
    Effect.gen(function* () {
      const asked: Array<CloudQuery> = [];
      const opens: Array<string> = [];
      const services = yield* storeTestCloud();
      const heard = yield* callCloud(member, { ask: LOGS }).pipe(runtime(connected(asked), opens), Effect.provide(services));
      expect(heard).toEqual({ ok: true, value: TAIL });
      expect(asked).toEqual([LOGS]);
      expect(opens).toEqual([ORGANIZATION]);
    }));

  it.live("scrubs a logs ask under every name the Store holds Sealed, whatever the ask carries", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      const store = yield* cloudStore.pipe(Effect.provide(services));
      const production = { project: "shop", environment: null };
      yield* Effect.promise(async () => {
        await store.write(ORGANIZATION, { command: "create_project", id: PROJECT, name: "shop", default_environment: ENVIRONMENT });
        await store.write(ORGANIZATION, { command: "create_service", id: SERVICE, environment: production, name: "web", image: "whoami:1" });
        await store.write(ORGANIZATION, { command: "edit", environment: production, expect: null, changes: [
          { op: "set", path: "web.env.TOKEN", value: sealedSecret(WEB, "TOKEN") },
          { op: "set", path: "web.env.OLD", value: sealedSecret(WEB, "OLD") },
          { op: "set", path: "web.env.PLAIN", value: "hello-plain" },
        ] });
        await store.write(ORGANIZATION, { command: "edit", environment: production, expect: null, changes: [{ op: "unset", path: "web.env.OLD" }] });
      });
      yield* Effect.gen(function* () {
        const { drizzle } = yield* Database;
        yield* drizzle.execute(sql`insert into config_namespace (organization_id, namespace, environment_id) values (${ORGANIZATION}, 'shop-production', ${ENVIRONMENT})`);
      }).pipe(Effect.provide(services), Effect.orDie);
      const asked: Array<CloudQuery> = [];
      const sealeds: Array<ReadonlyArray<string> | null> = [];
      const heard = (query: CloudQuery) => callCloud(member, { ask: query }).pipe(runtime(connected(asked, [], sealeds), []), Effect.provide(services));

      const weakened = { ...LOGS, sealed: ["PLAIN"] } as CloudQuery;
      expect(yield* heard(LOGS)).toEqual({ ok: true, value: TAIL });
      expect(yield* heard(weakened)).toEqual({ ok: true, value: TAIL });
      expect(yield* heard({ ...LOGS, namespace: "someone-elses" })).toEqual({ ok: true, value: TAIL });
      expect(sealeds).toEqual([["OLD", "TOKEN"], ["OLD", "TOKEN"], null]);
      expect(asked).toHaveLength(3);
    }));

  it.live("reruns a logs ask under a name sealed while the runtime was being read, and withholds one that never settles", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      const store = yield* cloudStore.pipe(Effect.provide(services));
      const production = { project: "shop", environment: null };
      const seal = (name: string) => store.write(ORGANIZATION, {
        command: "edit", environment: production, expect: null, changes: [{ op: "set", path: `web.env.${name}`, value: sealedSecret(WEB, name) }],
      });
      yield* Effect.promise(async () => {
        await store.write(ORGANIZATION, { command: "create_project", id: PROJECT, name: "shop", default_environment: ENVIRONMENT });
        await store.write(ORGANIZATION, { command: "create_service", id: SERVICE, environment: production, name: "web", image: "whoami:1" });
        await seal("TOKEN");
      });
      yield* Effect.gen(function* () {
        const { drizzle } = yield* Database;
        yield* drizzle.execute(sql`insert into config_namespace (organization_id, namespace, environment_id) values (${ORGANIZATION}, 'shop-production', ${ENVIRONMENT})`);
      }).pipe(Effect.provide(services), Effect.orDie);
      const sealeds: Array<ReadonlyArray<string> | null> = [];
      const racing = (sealing: Array<string | null>): ScopedRuntimeClientSession => ({
        status: "connected",
        connected: asTestDouble<PloyzSession>()({
          agentAsk: (_query: CloudQuery, _readable: ReadonlyArray<string> | null, sealed: ReadonlyArray<string> | null) => Effect.promise(async () => {
            sealeds.push(sealed);
            const name = sealing.shift();
            if (name !== undefined && name !== null) await seal(name);
            return TAIL;
          }),
        }),
      });
      const heard = (sealing: Array<string | null>) => callCloud(member, { ask: LOGS }).pipe(runtime(racing(sealing), []), Effect.provide(services));

      expect(yield* heard(["DURING", null])).toEqual({ ok: true, value: TAIL });
      expect(sealeds).toEqual([["TOKEN"], ["DURING", "TOKEN"]]);

      sealeds.length = 0;
      expect(yield* heard(["ONE", "TWO", "THREE"])).toEqual({
        ok: false,
        refusal: { code: "unavailable", message: "Sealed variables kept changing while the logs were read. Ask again.", details: null },
      });
      expect(sealeds).toEqual([["DURING", "TOKEN"], ["DURING", "ONE", "TOKEN"], ["DURING", "ONE", "TOKEN", "TWO"]]);
    }));

  // Catches support reading logs the Organization keeps from it, which the Logs tab refuses.
  it.live("refuses support the Organization doesn't show its logs to, before opening the runtime", () =>
    Effect.gen(function* () {
      const asked: Array<CloudQuery> = [];
      const opens: Array<string> = [];
      const support: Caller = { ...member, role: null, support: { variables: true, configs: true, logs: false } };
      const services = yield* storeTestCloud();
      const heard = yield* callCloud(support, { ask: LOGS }).pipe(runtime(connected(asked), opens), Effect.provide(services));
      expect(heard).toEqual({
        ok: false,
        refusal: { code: "forbidden", message: "This Organization doesn't show its container logs to Ployz support.", details: null },
      });
      const showsLogs = { ...member, support: { variables: false, configs: false, logs: true } };
      const serverLogs = yield* callCloud(showsLogs, { ask: { query: "server_logs", process: "ployz", server: null, lines: 100, since: null } })
        .pipe(runtime(connected(asked), opens), Effect.provide(services));
      expect(serverLogs).toEqual({
        ok: false,
        refusal: { code: "forbidden", message: "Ployz support can't read an Organization's Server logs.", details: null },
      });
      expect(opens).toEqual([]);
      expect(asked).toEqual([]);

      const allowed = yield* callCloud(showsLogs, { ask: LOGS })
        .pipe(runtime(connected(asked), opens), Effect.provide(services));
      expect(allowed).toEqual({ ok: true, value: TAIL });
    }));

  // Catches a read token kept from logs it may read, or a token held to staging reading production's logs, metrics or containers.
  it.live("lets a Read only token read logs, and holds a Deploy token's logs, metrics, containers, copies and Server metrics to its Environment", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      const store = yield* cloudStore.pipe(Effect.provide(services));
      const STAGING = "00000000-0000-4000-8000-00000000c109";
      yield* Effect.promise(async () => {
        await store.write(ORGANIZATION, { command: "create_project", id: PROJECT, name: "shop", default_environment: ENVIRONMENT });
        await store.write(ORGANIZATION, { command: "create_environment", id: STAGING, project: "shop", name: "staging" });
      });
      yield* Effect.gen(function* () {
        const { drizzle } = yield* Database;
        yield* drizzle.execute(sql`
          insert into config_namespace (organization_id, namespace, environment_id)
          values (${ORGANIZATION}, 'shop-prod', ${ENVIRONMENT}), (${ORGANIZATION}, 'shop-staging', ${STAGING})`);
      }).pipe(Effect.provide(services), Effect.orDie);
      const [production, staging] = ["shop-prod", "shop-staging"];
      const token = (grant: ReturnType<typeof presetGrant>): Caller => ({ ...member, credential: { kind: "token", id: "t", grant } });
      const reader = token(presetGrant({ preset: "read" }));
      const deployer = token(presetGrant({ preset: "deploy", environment: "staging" }));
      const logs = (namespace: string): CloudQuery => ({ ...LOGS, namespace });
      const metrics = (namespace: string): CloudQuery => ({ query: "metrics", subject: { kind: "service", namespace, service: "web" }, since: { kind: "minutes_ago", minutes: 10 }, deployment: null });
      const containers = (namespace: string): CloudQuery => ({ query: "containers", namespace, service: null });
      const refused = { ok: false, refusal: { code: "forbidden", message: "This token can deploy only to staging.", details: { environments: ["staging"] } } };
      const asked: Array<CloudQuery> = [];
      const heard = (caller: Caller, query: CloudQuery) => callCloud(caller, { ask: query }).pipe(runtime(connected(asked), []), Effect.provide(services));

      expect(yield* heard(reader, logs(production))).toEqual({ ok: true, value: TAIL });
      expect(yield* heard(deployer, logs(staging))).toEqual({ ok: true, value: TAIL });
      expect(asked).toEqual([logs(production), logs(staging)]);
      const copies: CloudQuery = { query: "copies", namespace: production, volume: null };
      for (const query of [logs(production), metrics(production), containers(production), copies]) {
        expect(yield* heard(deployer, query)).toEqual(refused);
      }
      expect(yield* heard(deployer, { query: "server_logs", process: "ployz", server: null, lines: 100, since: null })).toEqual(refused);
      expect(asked).toHaveLength(2);
      expect(yield* heard(deployer, metrics(staging))).toEqual({ ok: true, value: TAIL });
      expect(yield* heard(deployer, containers(staging))).toEqual({ ok: true, value: TAIL });

      // A Server's metrics keep its host totals for the Deploy token, but describe only the Services it may read.
      const readables: Array<ReadonlyArray<string> | null> = [];
      const serverMetrics: CloudQuery = { query: "metrics", subject: { kind: "server", server: { machine: MACHINE, name: "web-2" } }, since: { kind: "minutes_ago", minutes: 10 }, deployment: null };
      for (const caller of [member, reader, deployer]) {
        expect(yield* callCloud(caller, { ask: serverMetrics }).pipe(runtime(connected([], readables), []), Effect.provide(services))).toEqual({ ok: true, value: TAIL });
      }
      expect(readables).toEqual([null, null, [staging]]);
    }));

  const REMOVE: Act = {
    attempt: "a".repeat(32),
    digest: "b".repeat(64),
    action: { action: "remove_server", server: { machine: MACHINE, name: "web-2" }, loss: null },
  };

  // Catches an Organization that never added a Server told its Servers aren't answering, or its Server list refused.
  it.live("lists no Servers for an Organization with none, and tells it to add one for a logs ask and a removal", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      const noServers = {
        ok: false,
        refusal: { code: "conflict", message: NO_SERVERS_MESSAGE, details: { fix: { fix: "connect_servers" } } },
      };
      for (const opened of [{ status: "unreachable", error: null }, { status: "no_connection" }] as const) {
        expect(yield* callCloud(member, { ask: { query: "servers", only: null } }).pipe(runtime(opened, []), Effect.provide(services))).toEqual({
          ok: true,
          value: { servers: [], seen: expect.objectContaining({ from: "cloud_cache", unanswered: [] }) },
        });
        expect(yield* callCloud(member, { ask: LOGS }).pipe(runtime(opened, []), Effect.provide(services))).toEqual(noServers);
        expect(yield* callCloud(member, { act: REMOVE }).pipe(runtime(opened, []), Effect.provide(services))).toEqual(noServers);
      }
    }));

  // Catches a token without the permission told the Servers are missing or down, before it hears it may not act.
  it.live("refuses a token its grant before looking for Servers, for a volume move or mirror and a removal", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      const deployer: Caller = { ...member, credential: { kind: "token", id: "t", grant: presetGrant({ preset: "deploy", environment: "staging" }) } };
      const to: ServerRef = { machine: MACHINE, name: "web-2" };
      const volume = (run: VolumeRun): Act => ({ attempt: "c".repeat(32), digest: "d".repeat(64), action: { action: "volume", volume: VOLUME, run } });
      const lacking = (permission: string) =>
        ({ ok: false, refusal: { code: "forbidden", message: `This token's grant doesn't include ${permission}.`, details: { permission } } });
      for (const opened of [{ status: "unreachable", error: null }, { status: "no_connection" }] as const) {
        const opens: Array<string> = [];
        for (const run of [{ run: "move", to }, { run: "mirror", to }] as const) {
          expect(yield* callCloud(deployer, { act: volume(run) }).pipe(runtime(opened, opens), Effect.provide(services))).toEqual(lacking("services:control"));
        }
        expect(yield* callCloud(deployer, { act: REMOVE }).pipe(runtime(opened, opens), Effect.provide(services))).toEqual(lacking("servers:remove"));
        expect(opens).toEqual([]);
      }
    }));

  // Catches an unreachable Cluster surfacing as a crash, an empty tail, or the no-Servers refusal.
  it.live("refuses a logs ask clearly when the Organization's Servers aren't connected", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      yield* Effect.all([seedStoreOrganization(ORGANIZATION), enrollStoreServer(ORGANIZATION)]).pipe(Effect.provide(services));
      for (const opened of [{ status: "unreachable", error: null }, { status: "no_connection" }] as const) {
        const heard = yield* callCloud(member, { ask: LOGS }).pipe(runtime(opened, []), Effect.provide(services));
        expect(heard).toEqual({
          ok: false,
          refusal: { code: "unavailable", message: UNREACHABLE_MESSAGE, details: { fix: { fix: "connect_servers" } } },
        });
      }
    }));

  // Catches a failed read before the removal reported as a write that may have reached the Servers.
  it.live("says nothing was written when a removal fails while reading what it would lose", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      const reading: ScopedRuntimeClientSession = {
        status: "connected",
        connected: asTestDouble<PloyzSession>()({
          dataLossIfMachineRemoved: () => Effect.fail(new Error("the Server hung up")),
        }),
      };
      const heard = yield* callCloud(member, { act: REMOVE }).pipe(runtime(reading, []), Effect.provide(services));
      expect(heard).toEqual({ ok: false, refusal: { code: "unavailable", message: "Cloud couldn't answer this. Try again.", details: null } });
    }));

  // Catches a policy change sending values the agent left alone, or answering as a run already done.
  it.live("queues a Server policy change with only the values the agent set, and answers it accepted", () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "call-cloud-test" });
      const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const services = yield* storeTestCloud({ inngest });
      const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      const caller: Caller = { ...member, userId };
      const set = {
        action: "set_server",
        server: { machine: MACHINE, name: "web-2" },
        accepts_builds: null,
        accepts_services: false,
        accepts_ingress: false,
        build_concurrency: { action: "set", value: 2 },
      } as const satisfies Act["action"];
      const act: Act = {
        attempt: "c".repeat(32),
        digest: "d".repeat(64),
        action: set,
      };
      const attempt = attemptIdOf(ORGANIZATION, "server_set", act.attempt);
      // Catches an attempt another Organization's same request key would share.
      expect(attemptIdOf(DRAIN, "server_set", act.attempt)).not.toBe(attempt);
      const heard = yield* callCloud(caller, { act }).pipe(runtime(observing(true), []), Effect.provide(services));
      expect(heard).toEqual({ ok: true, value: expect.objectContaining({ attempt, run: "set web-2", state: "accepted" }) });
      const id = `server-policy-${attempt}`;
      expect(send.mock.calls).toEqual([[expect.objectContaining({
        id,
        data: { organizationId: ORGANIZATION, machineId: MACHINE, change: { acceptsServices: false, acceptsIngress: false, buildConcurrency: 2 } },
      })]]);
      // Catches a replayed act queuing the change again, or answering a different time, however long after the first.
      const [admitted] = yield* Effect.gen(function* () {
        const { drizzle } = yield* Database;
        return yield* drizzle.update(operationRequest).set({ createdAt: sql`${operationRequest.createdAt} - interval '1 hour'` })
          .where(eq(operationRequest.organizationId, ORGANIZATION)).returning({ createdAt: operationRequest.createdAt });
      }).pipe(Effect.provide(services));
      const replayed = yield* callCloud(caller, { act }).pipe(runtime(observing(true), []), Effect.provide(services));
      expect(replayed).toEqual({ ...heard, value: { ...asRecord(heard.ok ? heard.value : null), requested_at: Math.floor((admitted?.createdAt.getTime() ?? 0) / 1000) } });
      expect(send).toHaveBeenCalledTimes(1);
      // Catches the same attempt reused for another change, which would answer as the first one ran.
      const reused = yield* callCloud(caller, { act: { ...act, action: { ...set, accepts_services: true } } })
        .pipe(runtime(observing(true), []), Effect.provide(services));
      expect(reused).toEqual({ ok: false, refusal: expect.objectContaining({ code: "conflict" }) });
      expect(send).toHaveBeenCalledTimes(1);
    }));

  // Catches the last Server taking web traffic turned off without asking, or the change queued before a human approves.
  it.live("asks a human before turning web traffic off on the last Server that takes it, and queues the change once approved", () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "call-cloud-test" });
      const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const services = yield* storeTestCloud({ inngest });
      const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      const caller: Caller = { ...member, userId };
      const act: Act = {
        attempt: "f".repeat(32),
        digest: "d".repeat(64),
        action: {
          action: "set_server",
          server: { machine: MACHINE, name: "web-2" },
          accepts_builds: null,
          accepts_services: null,
          accepts_ingress: false,
          build_concurrency: { action: "keep" },
        },
      };

      const asked = yield* callCloud(caller, { act }).pipe(runtime(observing(false), []), Effect.provide(services));
      expect(asked).toEqual({ ok: false, refusal: expect.objectContaining({
        code: "approval_required",
        details: expect.objectContaining({
          effects: [{ kind: "removes_domain", name: "shop.example.com", node: "shop.example.com", path: "domains/shop.example.com" }],
          operation: { verb: "set", name: "web-2", subject: `server:${MACHINE}` },
        }),
      }) });
      expect(send).not.toHaveBeenCalled();

      const { approval, approval_id } = Schema.decodeUnknownSync(Asked)(asked.ok ? null : asked.refusal.details);
      const decided = yield* decideApproval({ ...caller, via: "dashboard" }, approval_id, { approve: { digest: approval } }).pipe(Effect.provide(services));
      expect(decided).toMatchObject({ ok: true });
      const approved = yield* callCloud(caller, { act }, approval_id).pipe(runtime(observing(false), []), Effect.provide(services));
      expect(approved).toEqual({ ok: true, value: expect.objectContaining({ run: "set web-2", state: "accepted" }) });
      expect(send).toHaveBeenCalledTimes(1);
    }));

  const deployedWeb = Effect.gen(function* () {
    const inngest = new Inngest({ id: "call-cloud-test" });
    const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
    const services = yield* storeTestCloud({ inngest });
    const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
    const store = yield* seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }).pipe(Effect.provide(services));
    yield* Effect.promise(() => store.write(ORGANIZATION, {
      command: "admit", admit: "deploy", id: DEPLOYMENT, environment: { project: null, environment: null }, services: [], version: null, accept_volume_loss: [],
    }, acmeWeb()));
    const { drizzle } = yield* Database.pipe(Effect.provide(services));
    yield* drizzle.execute(sql`update config_deployment set status = 'applied' where id = ${DEPLOYMENT}`);
    yield* drizzle.execute(sql`insert into config_applied (environment_id, node_id, organization_id, deployment_id, node_type, node)
      select environment_id, service->>'id', organization_id, ${DEPLOYMENT}, 'service', service::text
      from config_saved, lateral jsonb_array_elements(intent::jsonb->'services') service where environment_id = ${ENVIRONMENT}`);
    const caller: Caller = { ...member, userId };
    return { services, drizzle, caller, send, store };
  });

  // Catches an agent stopping or restarting a Service without asking, or a start (which interrupts nothing) made to ask.
  it.live("asks a human before an agent stops or restarts a Service, and runs a start straight away", () =>
    Effect.gen(function* () {
      const { services, drizzle, caller, send } = yield* deployedWeb;
      const act = (verb: ServiceVerb, attempt: string, only: ServerRef | null = null): Act => ({
        attempt: attempt.repeat(32),
        digest: "d".repeat(64),
        action: { action: "service", environment: ENVIRONMENT, service: "web", only, verb },
      });

      for (const [verb, attempt, only, name] of [
        ["stop", "1", null, "web in shop/production"],
        ["restart", "2", null, "web in shop/production"],
        ["stop", "4", { machine: MACHINE, name: "fra-1" }, "web in shop/production on fra-1"],
      ] as const) {
        const asked = yield* callCloud(caller, { act: act(verb, attempt, only) }).pipe(Effect.provide(services));
        expect(asked).toEqual({ ok: false, refusal: expect.objectContaining({
          code: "approval_required",
          details: expect.objectContaining({ operation: expect.objectContaining({ verb, name }) }),
        }) });
      }
      expect(yield* drizzle.select().from(serviceAct)).toEqual([]);

      const started = yield* callCloud(caller, { act: act("start", "3") }).pipe(Effect.provide(services));
      expect(started).toEqual({ ok: true, value: expect.objectContaining({ state: "accepted" }) });
      expect((yield* drizzle.select().from(serviceAct)).map((row) => row.verb)).toEqual(["start"]);
      expect(send).toHaveBeenCalledTimes(1);
    }));

  // Catches a quick mirror sync, which asks no one, running under any approval carried into it.
  it.live("a quick mirror sync refuses an approval carried into it for another operation, target or session, and takes one for exactly it", () =>
    Effect.gen(function* () {
      const { services, drizzle, caller } = yield* deployedWeb;
      const approved = (command: string, subject: string, credentialId: string) => drizzle.insert(operationApprovals).values({
        organizationId: ORGANIZATION, subject, requestedByUserId: caller.userId, via: "agent", credentialKind: "session", credentialId,
        command, digest: `${command}:${subject}`, status: "approved", decidedByUserId: caller.userId, decidedAt: new Date(),
        review: asTestDouble<typeof operationApprovals.$inferInsert.review>()({ effects: [], operation: { verb: command, name: "data", preview: null } }),
      }).returning({ id: operationApprovals.id }).pipe(Effect.map(([row]) => row?.id ?? expect.fail("no approval")));
      const sync: Act = { attempt: "9".repeat(32), digest: "d".repeat(64), action: { action: "volume", volume: VOLUME, run: { run: "sync", full: false } } };
      const synced = (approvalId: string) => callCloud(caller, { act: sync }, approvalId).pipe(Effect.provide(services));
      const forbidden = (approval_id: string) => ({ ok: false, refusal: expect.objectContaining({ code: "forbidden", details: { approval_id } }) });

      for (const id of [
        yield* approved("release", `volume:${VOLUME}`, "session-1"),
        yield* approved("sync", "volume:00000000-0000-4000-8000-00000000c1ff", "session-1"),
        yield* approved("sync", `volume:${VOLUME}`, "session-2"),
      ]) expect(yield* synced(id)).toEqual(forbidden(id));
      // Its own approval passes, and the sync goes on to look for the Volume, which this test never made.
      const own = yield* synced(yield* approved("sync", `volume:${VOLUME}`, "session-1"));
      expect(own).toEqual({ ok: false, refusal: expect.objectContaining({ code: "not_found" }) });
    }));

  // Catches an unreadable Environment answering a Volume lookup with an opaque internal error.
  it.live("a Volume action refuses naming the Environment the Store can't read, when the Volume may be there", () =>
    Effect.gen(function* () {
      const { services, drizzle, caller } = yield* deployedWeb;
      yield* drizzle.execute(sql`update config_environment set working = '{"from_the_future":true}' where id = ${ENVIRONMENT}`);
      const sync: Act = { attempt: "8".repeat(32), digest: "d".repeat(64), action: { action: "volume", volume: VOLUME, run: { run: "sync", full: false } } };
      const refused = yield* callCloud(caller, { act: sync }).pipe(Effect.provide(services));
      expect(refused).toEqual({ ok: false, refusal: expect.objectContaining({
        code: "conflict",
        message: expect.stringContaining("can't read shop/production"),
      }) });
    }));

  // Catches one approved stop authorizing every later stop of the Service, or a dashboard Stop made to wait for a human.
  it.live("one approval runs one stop: a retry with its key replays it, a later stop asks again, and the dashboard never asks", () =>
    Effect.gen(function* () {
      const { services, drizzle, caller } = yield* deployedWeb;
      const stop = (attempt: string): Act => ({
        attempt: attempt.repeat(32),
        digest: "d".repeat(64),
        action: { action: "service", environment: ENVIRONMENT, service: "web", only: null, verb: "stop" },
      });

      const asked = yield* callCloud(caller, { act: stop("1") }).pipe(Effect.provide(services));
      const { approval, approval_id } = Schema.decodeUnknownSync(Asked)(asked.ok ? null : asked.refusal.details);
      expect(yield* decideApproval({ ...caller, via: "dashboard" }, approval_id, { approve: { digest: approval } }).pipe(Effect.provide(services))).toMatchObject({ ok: true });
      const ran = yield* callCloud(caller, { act: stop("2") }, approval_id).pipe(Effect.provide(services));
      expect(ran).toEqual({ ok: true, value: expect.objectContaining({ run: "stop web", state: "accepted" }) });
      const retried = yield* callCloud(caller, { act: stop("2") }, approval_id).pipe(Effect.provide(services));
      expect(retried).toEqual(ran);
      // Its key replays the stop only for the credential that asked: another one is refused, even on a replay.
      const elsewhere = yield* callCloud({ ...caller, credential: { kind: "session", id: "session-2" } }, { act: stop("2") }, approval_id).pipe(Effect.provide(services));
      expect(elsewhere).toEqual({ ok: false, refusal: expect.objectContaining({ code: "forbidden", details: { approval_id } }) });
      yield* drizzle.execute(sql`update service_act set state = 'done', ended_at = now()`);

      const again = yield* callCloud(caller, { act: stop("3") }, approval_id).pipe(Effect.provide(services));
      expect(again).toEqual({ ok: false, refusal: expect.objectContaining({ code: "approval_required" }) });
      const fresh = Schema.decodeUnknownSync(Asked)(again.ok ? null : again.refusal.details);
      expect(fresh.approval_id).not.toBe(approval_id);
      expect((yield* drizzle.select().from(serviceAct)).map((row) => row.state)).toEqual(["done"]);

      // A start asks no one, and still refuses the stop's approval carried into it.
      const start: Act = { ...stop("5"), action: { action: "service", environment: ENVIRONMENT, service: "web", only: null, verb: "start" } };
      const carried = yield* callCloud(caller, { act: start }, fresh.approval_id).pipe(Effect.provide(services));
      expect(carried).toEqual({ ok: false, refusal: expect.objectContaining({ code: "forbidden", details: { approval_id: fresh.approval_id } }) });

      const clicked = yield* callCloud({ ...caller, via: "dashboard" }, { act: stop("4") }).pipe(Effect.provide(services));
      expect(clicked).toEqual({ ok: true, value: expect.objectContaining({ run: "stop web", state: "accepted" }) });
    }));

  // Catches a human asked to approve a restart that can only be refused because another run holds the Service.
  it.live("a stop or restart while the Service is busy is refused before anyone is asked", () =>
    Effect.gen(function* () {
      const { services, drizzle, caller } = yield* deployedWeb;
      const act = (verb: ServiceVerb, attempt: string): Act => ({
        attempt: attempt.repeat(32),
        digest: "d".repeat(64),
        action: { action: "service", environment: ENVIRONMENT, service: "web", only: null, verb },
      });
      const stopped = yield* callCloud({ ...caller, via: "dashboard" }, { act: act("stop", "1") }).pipe(Effect.provide(services));
      expect(stopped).toEqual({ ok: true, value: expect.objectContaining({ run: "stop web", state: "accepted" }) });

      for (const [verb, attempt] of [["restart", "2"], ["stop", "3"]] as const) {
        const busy = yield* callCloud(caller, { act: act(verb, attempt) }).pipe(Effect.provide(services));
        expect(busy).toEqual({ ok: false, refusal: expect.objectContaining({ code: "conflict", message: "web is busy: stop web is still running." }) });
      }
      expect((yield* drizzle.select().from(serviceAct)).map((row) => row.verb)).toEqual(["stop"]);
    }));

  it.live("a member's stop approved before production was protected asks a protector once it is", () =>
    Effect.gen(function* () {
      const { services, drizzle, caller, store } = yield* deployedWeb;
      const agent: Caller = { ...caller, role: "member" };
      const stop = (attempt: string): Act => ({
        attempt: attempt.repeat(32),
        digest: "d".repeat(64),
        action: { action: "service", environment: ENVIRONMENT, service: "web", only: null, verb: "stop" },
      });

      const protect = (on: boolean) => Effect.promise(() => store.write(ORGANIZATION, { command: "set_environment_protected", environment: { project: null, environment: null }, protected: on }));
      yield* protect(false);
      const asked = yield* callCloud(agent, { act: stop("1") }).pipe(Effect.provide(services));
      const { approval, approval_id } = Schema.decodeUnknownSync(Asked)(asked.ok ? null : asked.refusal.details);
      expect(yield* decideApproval({ ...caller, via: "dashboard" }, approval_id, { approve: { digest: approval } }).pipe(Effect.provide(services))).toMatchObject({ ok: true });
      yield* protect(true);

      const guarded = yield* callCloud(agent, { act: stop("2") }, approval_id).pipe(Effect.provide(services));
      expect(guarded).toEqual({ ok: false, refusal: expect.objectContaining({ code: "approval_required" }) });
      const fresh = Schema.decodeUnknownSync(Asked)(guarded.ok ? null : guarded.refusal.details);
      expect(fresh.approval).not.toBe(approval);
      expect(yield* drizzle.select().from(serviceAct)).toEqual([]);
    }));

  // Catches an approval to stop web on one Server stopping it on another Server that has the same name.
  it.live("an approved stop on one Server asks again for another Server with the same name", () =>
    Effect.gen(function* () {
      const { services, drizzle, caller } = yield* deployedWeb;
      const stop = (attempt: string, machine: MachineId): Act => ({
        attempt: attempt.repeat(32),
        digest: "d".repeat(64),
        action: { action: "service", environment: ENVIRONMENT, service: "web", only: { machine, name: "fra-1" }, verb: "stop" },
      });

      const asked = yield* callCloud(caller, { act: stop("1", MACHINE) }).pipe(Effect.provide(services));
      const { approval, approval_id } = Schema.decodeUnknownSync(Asked)(asked.ok ? null : asked.refusal.details);
      expect(yield* decideApproval({ ...caller, via: "dashboard" }, approval_id, { approve: { digest: approval } }).pipe(Effect.provide(services))).toMatchObject({ ok: true });

      const elsewhere = yield* callCloud(caller, { act: stop("2", OTHER_MACHINE) }, approval_id).pipe(Effect.provide(services));
      expect(elsewhere).toEqual({ ok: false, refusal: expect.objectContaining({ code: "approval_required" }) });
      expect(yield* drizzle.select().from(serviceAct)).toEqual([]);

      const there = yield* callCloud(caller, { act: stop("3", MACHINE) }, approval_id).pipe(Effect.provide(services));
      expect(there).toEqual({ ok: true, value: expect.objectContaining({ run: "stop web", state: "accepted" }) });
      expect((yield* drizzle.select().from(serviceAct)).map((row) => row.only?.machine)).toEqual([MACHINE]);
    }));

  // Catches an approval asked before the digest named the machine, for a Server named like another Server's id,
  // stopping web on that other Server.
  it.live("an approval that hashed a Server's name never authorizes a stop on the Server whose id is that name", () =>
    Effect.gen(function* () {
      const { services, drizzle, caller } = yield* deployedWeb;
      const stop = (attempt: string, only: ServerRef): Act => ({
        attempt: attempt.repeat(32),
        digest: "d".repeat(64),
        action: { action: "service", environment: ENVIRONMENT, service: "web", only, verb: "stop" },
      });

      const asked = yield* callCloud(caller, { act: stop("1", { machine: MACHINE, name: OTHER_MACHINE }) }).pipe(Effect.provide(services));
      const { approval_id } = Schema.decodeUnknownSync(Asked)(asked.ok ? null : asked.refusal.details);
      const byName = operationDigest({
        subject: `service:${ENVIRONMENT}/web`,
        verb: "stop",
        preview: { environment: ENVIRONMENT, service: "web", only: OTHER_MACHINE },
        effects: [{ kind: "stops_service", name: "web", node: `${ENVIRONMENT}/web`, path: "services.web" }],
      });
      yield* drizzle.execute(sql`update operation_approvals set digest = ${byName} where id = ${approval_id}`);
      expect(yield* decideApproval({ ...caller, via: "dashboard" }, approval_id, { approve: { digest: byName } }).pipe(Effect.provide(services))).toMatchObject({ ok: true });

      const elsewhere = yield* callCloud(caller, { act: stop("2", { machine: OTHER_MACHINE, name: "fra-2" }) }, approval_id).pipe(Effect.provide(services));
      expect(elsewhere).toEqual({ ok: false, refusal: expect.objectContaining({ code: "approval_required" }) });
      expect(yield* drizzle.select().from(serviceAct)).toEqual([]);
    }));

  // Catches an approval for web off authorizing a change that also turns other policy off, or a reread that supersedes
  // an approval for the whole change.
  it.live("an approval for turning web traffic off covers no other value a replayed change sets, and one for the whole change does", () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "call-cloud-test" });
      const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const services = yield* storeTestCloud({ inngest });
      const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      const caller: Caller = { ...member, userId };
      const off = {
        action: "set_server",
        server: { machine: MACHINE, name: "web-2" },
        accepts_builds: null,
        accepts_services: null,
        accepts_ingress: false,
        build_concurrency: { action: "keep" },
      } as const satisfies Act["action"];
      const asked = yield* callCloud(caller, { act: { attempt: "1".repeat(32), digest: "d".repeat(64), action: off } })
        .pipe(runtime(observing(false), []), Effect.provide(services));
      const { approval, approval_id } = Schema.decodeUnknownSync(Asked)(asked.ok ? null : asked.refusal.details);
      expect(yield* decideApproval({ ...caller, via: "dashboard" }, approval_id, { approve: { digest: approval } }).pipe(Effect.provide(services))).toMatchObject({ ok: true });

      for (const [attempt, more] of [
        ["2", { accepts_services: false }],
        ["3", { accepts_builds: false }],
        ["4", { build_concurrency: { action: "set", value: 1 } }],
      ] as const) {
        const replayed = yield* callCloud(caller, { act: { attempt: attempt.repeat(32), digest: "d".repeat(64), action: { ...off, ...more } } }, approval_id)
          .pipe(runtime(observing(false), []), Effect.provide(services));
        expect(replayed).toEqual({ ok: false, refusal: expect.objectContaining({ code: "approval_required" }) });
      }
      expect(send).not.toHaveBeenCalled();

      const whole = { attempt: "5".repeat(32), digest: "d".repeat(64), action: { ...off, accepts_services: false } } as const satisfies Act;
      const askedWhole = yield* callCloud(caller, { act: whole }).pipe(runtime(observing(false), []), Effect.provide(services));
      const asking = Schema.decodeUnknownSync(Asked)(askedWhole.ok ? null : askedWhole.refusal.details);
      const reread = yield* freshApproval(ORGANIZATION, asking.approval_id).pipe(runtime(observing(false), []), Effect.provide(services));
      expect(reread).toMatchObject({ status: "pending" });
      const decided = yield* decideApproval({ ...caller, via: "dashboard" }, asking.approval_id, { approve: { digest: asking.approval } }, freshOperationDigest)
        .pipe(runtime(observing(false), []), Effect.provide(services));
      expect(decided).toMatchObject({ ok: true });
      const ran = yield* callCloud(caller, { act: whole }, asking.approval_id).pipe(runtime(observing(false), []), Effect.provide(services));
      expect(ran).toEqual({ ok: true, value: expect.objectContaining({ run: "set web-2", state: "accepted" }) });
      expect(send).toHaveBeenCalledTimes(1);
    }));
});
