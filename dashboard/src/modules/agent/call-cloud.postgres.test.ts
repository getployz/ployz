import { it } from "@effect/vitest";
import type { Act, CloudQuery, LogsAnswer, MachineId, RuntimeWatchView, ServerRef, ServiceVerb } from "@ployz/sdk";
import { sql } from "drizzle-orm";
import { Effect, Layer, Schema } from "effect";
import { Inngest } from "inngest";
import { describe, expect, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { callCloud, NO_SERVERS_MESSAGE } from "#/modules/agent/call-cloud.server";
import { decideApproval, operationDigest } from "#/modules/approvals/approvals.server";
import type { Caller } from "#/modules/identity/actor";
import { freshApproval, freshOperationDigest, UNREACHABLE_MESSAGE } from "#/modules/machines/server-operations.server";
import { serverDrainAttempt } from "#/modules/machines/tables";
import { attemptIdOf } from "#/modules/operations/keyed.server";
import { serviceAct } from "#/modules/service-act/tables";
import { OrganizationRuntime, type ScopedRuntimeClientSession } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { acmeWeb, enrollStoreServer, seedStoreGitService, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000c101";
const DRAIN = "00000000-0000-4000-8000-00000000c102";
const PROJECT = "00000000-0000-4000-8000-00000000c104";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000c105";
const SERVICE = "00000000-0000-4000-8000-00000000c106";
const DEPLOYMENT = "00000000-0000-4000-8000-00000000c107";
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
      const support: Caller = { ...member, role: null, support: { variables: true, configs: true, logs: false } };
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

  const REMOVE: Act = {
    attempt: "a".repeat(32),
    digest: "b".repeat(64),
    action: { action: "remove_server", server: { machine: MACHINE, name: "web-2" }, loss: null },
  };

  // Catches an Organization that never added a Server told its Servers aren't answering.
  it.live("tells an Organization with no Servers to add one, for a logs ask and a removal", () =>
    Effect.gen(function* () {
      const services = yield* Layer.build(yield* storeTestCloud());
      yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      const noServers = {
        ok: false,
        refusal: { code: "unavailable", message: NO_SERVERS_MESSAGE, details: { fix: { fix: "connect_servers" } } },
      };
      for (const opened of [{ status: "unreachable", error: null }, { status: "no_connection" }] as const) {
        expect(yield* callCloud(member, { ask: LOGS }).pipe(runtime(opened, []), Effect.provide(services))).toEqual(noServers);
        expect(yield* callCloud(member, { act: REMOVE }).pipe(runtime(opened, []), Effect.provide(services))).toEqual(noServers);
      }
    }));

  // Catches an unreachable Cluster surfacing as a crash, an empty tail, or the no-Servers refusal.
  it.live("refuses a logs ask clearly when the Organization's Servers aren't connected", () =>
    Effect.gen(function* () {
      const services = yield* Layer.build(yield* storeTestCloud());
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
      const services = yield* Layer.build(yield* storeTestCloud());
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
      const decided = yield* decideApproval({ ...caller, via: "dashboard" }, approval_id, { approve: { digest: approval } }).pipe(Effect.provide(services));
      expect(decided).toMatchObject({ ok: true });
      const approved = yield* callCloud(caller, { act }, approval_id).pipe(runtime(observing(false), []), Effect.provide(services));
      expect(approved).toEqual({ ok: true, value: expect.objectContaining({ run: "set web-2", state: "accepted" }) });
      expect(send).toHaveBeenCalledTimes(1);
    }));

  const deployedWeb = Effect.gen(function* () {
    const inngest = new Inngest({ id: "call-cloud-test" });
    const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
    const services = yield* Layer.build(yield* storeTestCloud({ inngest }));
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
    return { services, drizzle, caller, send };
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
          details: expect.objectContaining({ operation: { verb, name } }),
        }) });
      }
      expect(yield* drizzle.select().from(serviceAct)).toEqual([]);

      const started = yield* callCloud(caller, { act: act("start", "3") }).pipe(Effect.provide(services));
      expect(started).toEqual({ ok: true, value: expect.objectContaining({ state: "accepted" }) });
      expect((yield* drizzle.select().from(serviceAct)).map((row) => row.verb)).toEqual(["start"]);
      expect(send).toHaveBeenCalledTimes(1);
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
      expect(yield* decideApproval(caller, approval_id, { approve: { digest: approval } }).pipe(Effect.provide(services))).toMatchObject({ ok: true });
      const ran = yield* callCloud(caller, { act: stop("2") }, approval_id).pipe(Effect.provide(services));
      expect(ran).toEqual({ ok: true, value: expect.objectContaining({ run: "stop web", state: "accepted" }) });
      const retried = yield* callCloud(caller, { act: stop("2") }, approval_id).pipe(Effect.provide(services));
      expect(retried).toEqual(ran);
      yield* drizzle.execute(sql`update service_act set state = 'done', ended_at = now()`);

      const again = yield* callCloud(caller, { act: stop("3") }, approval_id).pipe(Effect.provide(services));
      expect(again).toEqual({ ok: false, refusal: expect.objectContaining({ code: "approval_required" }) });
      const fresh = Schema.decodeUnknownSync(Asked)(again.ok ? null : again.refusal.details);
      expect(fresh.approval_id).not.toBe(approval_id);
      expect((yield* drizzle.select().from(serviceAct)).map((row) => row.state)).toEqual(["done"]);

      const clicked = yield* callCloud({ ...caller, via: "dashboard" }, { act: stop("4") }).pipe(Effect.provide(services));
      expect(clicked).toEqual({ ok: true, value: expect.objectContaining({ run: "stop web", state: "accepted" }) });
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
      expect(yield* decideApproval(caller, approval_id, { approve: { digest: approval } }).pipe(Effect.provide(services))).toMatchObject({ ok: true });

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
        verb: "stop",
        preview: { environment: ENVIRONMENT, service: "web", only: OTHER_MACHINE },
        effects: [{ kind: "stops_service", name: "web", node: `${ENVIRONMENT}/web`, path: "services.web" }],
      });
      yield* drizzle.execute(sql`update operation_approvals set digest = ${byName} where id = ${approval_id}`);
      expect(yield* decideApproval(caller, approval_id, { approve: { digest: byName } }).pipe(Effect.provide(services))).toMatchObject({ ok: true });

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
