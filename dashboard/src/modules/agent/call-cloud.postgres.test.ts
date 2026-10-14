import { it } from "@effect/vitest";
import type { Act, CloudQuery, LogsAnswer, MachineId } from "@ployz/sdk";
import { Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { describe, expect, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { callCloud } from "#/modules/agent/call-cloud.server";
import type { Caller } from "#/modules/identity/actor";
import { UNREACHABLE_MESSAGE } from "#/modules/machines/server-operations.server";
import { serverDrainAttempt } from "#/modules/machines/tables";
import { OrganizationRuntime, type ScopedRuntimeClientSession } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000c101";
const DRAIN = "00000000-0000-4000-8000-00000000c102";
const MACHINE = "a".repeat(32) as MachineId;

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
      const act: Act = {
        attempt: "c".repeat(32),
        digest: "d".repeat(64),
        action: {
          action: "set_server",
          server: { machine: MACHINE, name: "web-2" },
          accepts_builds: null,
          accepts_services: false,
          accepts_ingress: false,
          build_concurrency: { action: "set", value: 2 },
        },
      };
      const heard = yield* callCloud(caller, { act }).pipe(Effect.provide(services));
      expect(heard).toEqual({ ok: true, value: expect.objectContaining({ run: "set web-2", state: "accepted" }) });
      expect(send.mock.calls).toEqual([[expect.objectContaining({
        data: { organizationId: ORGANIZATION, machineId: MACHINE, change: { acceptsServices: false, acceptsIngress: false, buildConcurrency: 2 } },
      })]]);
      // Catches a replayed act queuing the change again: Inngest runs one event per id.
      yield* callCloud(caller, { act }).pipe(Effect.provide(services));
      const ids = send.mock.calls.map(([event]) => (event as { id?: string }).id);
      expect(ids).toEqual([`server-policy-${act.attempt}`, `server-policy-${act.attempt}`]);
    }));
});
