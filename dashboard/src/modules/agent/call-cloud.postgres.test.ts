import { it } from "@effect/vitest";
import type { Act, CloudQuery, LogsAnswer, MachineId, RuntimeWatchView } from "@ployz/sdk";
import { Effect, Layer, Schema } from "effect";
import { Inngest } from "inngest";
import { describe, expect, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { callCloud } from "#/modules/agent/call-cloud.server";
import { decideApproval } from "#/modules/approvals/approvals.server";
import type { Caller } from "#/modules/identity/actor";
import { freshApproval, freshOperationDigest, UNREACHABLE_MESSAGE } from "#/modules/machines/server-operations.server";
import { serverDrainAttempt } from "#/modules/machines/tables";
import { attemptIdOf } from "#/modules/operations/keyed.server";
import { OrganizationRuntime, type ScopedRuntimeClientSession } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000c101";
const DRAIN = "00000000-0000-4000-8000-00000000c102";
const MACHINE = "a".repeat(32) as MachineId;
const OTHER_MACHINE = "e".repeat(32) as MachineId;
const Asked = Schema.Struct({ approval: Schema.String, approval_id: Schema.String });

describe("callCloud", () => {
  it.live("answers an operations query as OperationsAnswer, the shape the agent tools decode", () =>
    Effect.gen(function* () {
      const services = yield* Layer.build(yield* storeTestCloud());
      const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      const { drizzle } = yield* Database.pipe(Effect.provide(services));
      yield* drizzle.insert(serverDrainAttempt).values({ id: DRAIN, organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId });
      const caller: Caller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "session-1" }, support: null, via: "agent" };

      const heard = yield* callCloud(caller, { ask: { query: "operations", subject: { kind: "server", machine: MACHINE }, attempt: null, limit: 5 } })
        .pipe(Effect.provide(services));

      expect(heard).toEqual({
        ok: true,
        value: { operations: [expect.objectContaining({ attempt: DRAIN, run: `drain ${MACHINE}`, state: "accepted" })] },
      });
    }));

  const LOGS: CloudQuery = {
    query: "logs",
    environment: "00000000-0000-4000-8000-00000000c103",
    namespace: "shop-production",
    service: "web",
    deployment: null,
    server: null,
    lines: 50,
    since: { secs: 600, nanos: 0 },
    plain: { PORT: "8080" },
  };
  const TAIL: LogsAnswer = {
    servers: [{ server: "server-1", text: { status: "shown", lines: ["listening on :8080\n", "token [secret TOKEN]\n"], cut: false, redacted: 1 } }],
    seen: { seen_by: null, observed_at: 1, read_at: 1, age_seconds: 0, from: "server", unanswered: [] },
    notes: [],
  };
  const member: Caller = { userId: "user-1", organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "session-1" }, support: null, via: "agent" };

  const runtime = (opened: ScopedRuntimeClientSession, opens: Array<string>) => Effect.provideService(OrganizationRuntime, {
    cancel: () => Effect.void,
    open: (organizationId: string) => Effect.sync(() => {
      opens.push(organizationId);
      return opened;
    }),
  });

  const connected = (asked: Array<CloudQuery>): ScopedRuntimeClientSession => ({
    status: "connected",
    connected: asTestDouble<PloyzSession>()({
      agentAsk: (query: CloudQuery) => Effect.sync(() => {
        asked.push(query);
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
      const services = yield* Layer.build(yield* storeTestCloud());
      const heard = yield* callCloud(member, { ask: LOGS }).pipe(runtime(connected(asked), opens), Effect.provide(services));
      expect(heard).toEqual({ ok: true, value: TAIL });
      expect(asked).toEqual([LOGS]);
      expect(opens).toEqual([ORGANIZATION]);
    }));

  // Catches support reading logs the Organization keeps from it, which the Logs tab refuses.
  it.live("refuses support the Organization doesn't show its logs to, before opening the runtime", () =>
    Effect.gen(function* () {
      const asked: Array<CloudQuery> = [];
      const opens: Array<string> = [];
      const support: Caller = { ...member, support: { variables: true, configs: true, logs: false } };
      const services = yield* Layer.build(yield* storeTestCloud());
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

  // Catches an unreachable Cluster surfacing as a crash or an empty tail instead of a refusal naming the fix.
  it.live("refuses a logs ask clearly when the Organization's Servers aren't connected", () =>
    Effect.gen(function* () {
      const services = yield* Layer.build(yield* storeTestCloud());
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
      const services = yield* Layer.build(yield* storeTestCloud());
      const reading: ScopedRuntimeClientSession = {
        status: "connected",
        connected: asTestDouble<PloyzSession>()({
          dataLossIfMachineRemoved: () => Effect.fail(new Error("the Server hung up")),
        }),
      };
      const act: Act = {
        attempt: "a".repeat(32),
        digest: "b".repeat(64),
        action: { action: "remove_server", server: { machine: MACHINE, name: "web-2" }, loss: null },
      };
      const heard = yield* callCloud(member, { act }).pipe(runtime(reading, []), Effect.provide(services));
      expect(heard).toEqual({ ok: false, refusal: { code: "unavailable", message: "Cloud couldn't answer this. Try again.", details: null } });
    }));

  // Catches a policy change sending values the agent left alone, or answering as a run already done.
  it.live("queues a Server policy change with only the values the agent set, and answers it accepted", () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "call-cloud-test" });
      const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const services = yield* Layer.build(yield* storeTestCloud({ inngest }));
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
      yield* Effect.sleep("1100 millis");
      const replayed = yield* callCloud(caller, { act }).pipe(runtime(observing(true), []), Effect.provide(services));
      expect(replayed).toEqual(heard);
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
      const services = yield* Layer.build(yield* storeTestCloud({ inngest }));
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
          operation: { verb: "set", name: "web-2" },
        }),
      }) });
      expect(send).not.toHaveBeenCalled();

      const { approval, approval_id } = Schema.decodeUnknownSync(Asked)(asked.ok ? null : asked.refusal.details);
      const decided = yield* decideApproval(caller, approval_id, { approve: { digest: approval } }).pipe(Effect.provide(services));
      expect(decided).toMatchObject({ ok: true });
      const approved = yield* callCloud(caller, { act }, approval_id).pipe(runtime(observing(false), []), Effect.provide(services));
      expect(approved).toEqual({ ok: true, value: expect.objectContaining({ run: "set web-2", state: "accepted" }) });
      expect(send).toHaveBeenCalledTimes(1);
    }));

  // Catches an approval for web off authorizing a change that also turns other policy off, or a reread that supersedes
  // an approval for the whole change.
  it.live("an approval for turning web traffic off covers no other value a replayed change sets, and one for the whole change does", () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "call-cloud-test" });
      const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const services = yield* Layer.build(yield* storeTestCloud({ inngest }));
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
      expect(yield* decideApproval(caller, approval_id, { approve: { digest: approval } }).pipe(Effect.provide(services))).toMatchObject({ ok: true });

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
      const decided = yield* decideApproval(caller, asking.approval_id, { approve: { digest: asking.approval } }, freshOperationDigest)
        .pipe(runtime(observing(false), []), Effect.provide(services));
      expect(decided).toMatchObject({ ok: true });
      const ran = yield* callCloud(caller, { act: whole }, asking.approval_id).pipe(runtime(observing(false), []), Effect.provide(services));
      expect(ran).toEqual({ ok: true, value: expect.objectContaining({ run: "set web-2", state: "accepted" }) });
      expect(send).toHaveBeenCalledTimes(1);
    }));
});
