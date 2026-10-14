import { it } from "@effect/vitest";
import type { Act, CloudQuery, LogsAnswer, MachineId } from "@ployz/sdk";
import { Effect, Layer } from "effect";
import { describe, expect } from "vitest";
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
      const caller: Caller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "session-1" }, role: "owner", support: null, via: "agent" };

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
  const member: Caller = { userId: "user-1", organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "session-1" }, role: "owner", support: null, via: "agent" };

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
      const support: Caller = { ...member, role: null, support: { variables: true, configs: true, logs: false } };
      const services = yield* Layer.build(yield* storeTestCloud());
      const heard = yield* callCloud(support, { ask: LOGS }).pipe(runtime(connected(asked), opens), Effect.provide(services));
      expect(heard).toEqual({
        ok: false,
        refusal: { code: "forbidden", message: "This Organization doesn't show its container logs to Ployz support.", details: null },
      });
      expect(opens).toEqual([]);
      expect(asked).toEqual([]);

      const allowed = yield* callCloud({ ...member, support: { variables: false, configs: false, logs: true } }, { ask: LOGS })
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
});
