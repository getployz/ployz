import "@tanstack/react-start/server-only";
import type { Operation, ServerRef, ServiceVerb } from "@ployz/sdk";
import { and, desc, eq, inArray, isNull, lt, or, sql } from "drizzle-orm";
import { Data, Effect } from "effect";
import { createServiceActRequestedEvent, type ServiceActRequestedEventData } from "#/modules/inngest/events";
import type { Admission } from "#/modules/operations/keyed.server";
import { enqueueEvent } from "#/modules/operations/outbox.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { rpcErrorCode } from "#/modules/runtime/ployz.server";
import {
  type ActiveServiceActState,
  alreadyThere,
  endOf,
  legOf,
  legsOf,
  type LegStep,
  mayStart,
  SERVICE_ACT_PENDING_LIMIT_MS,
  SERVICE_ACT_RUNNING_LIMIT_MS,
  type ServiceActBefore,
  type ServiceActLeg,
  type ServiceActState,
} from "#/modules/service-act/service-act";
import { serviceAct } from "#/modules/service-act/tables";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";

type ActRow = typeof serviceAct.$inferSelect;
type ActRequest = ServiceActRequestedEventData;

const ACTIVE_STATES = ["accepted", "running"] as const satisfies readonly ActiveServiceActState[];

export class ServiceActUnreachable extends Data.TaggedError("ServiceActUnreachable")<{ readonly cause: unknown }> {}

const seconds = (at: Date) => Math.floor(at.getTime() / 1000);

export const operationOf = (row: ActRow): Operation => ({
  attempt: row.id,
  run: `${row.verb} ${row.service}`,
  state: row.state,
  requested_at: seconds(row.requestedAt),
  ended_at: row.endedAt === null ? null : seconds(row.endedAt),
  reason: row.reason,
  servers: row.legs.map(legOf),
  kept: [],
  unanswered: row.before?.unanswered ?? [],
});

export const requestServiceAct = Effect.fn("ServiceAct.request")(function* (
  caller: { readonly organizationId: string; readonly userId: string },
  input: {
    readonly attemptId: string;
    readonly environmentId: string;
    readonly service: string;
    readonly namespace: string;
    readonly runtime: string;
    readonly verb: ServiceVerb;
    readonly only: ServerRef | null;
    readonly digest: string;
  },
) {
  const { drizzle } = yield* Database;
  const [inserted] = yield* drizzle.insert(serviceAct).values({
    id: input.attemptId,
    organizationId: caller.organizationId,
    environmentId: input.environmentId,
    service: input.service,
    namespace: input.namespace,
    runtime: input.runtime,
    verb: input.verb,
    only: input.only,
    digest: input.digest,
    requestedByUserId: caller.userId,
  }).onConflictDoNothing().returning();
  if (inserted !== undefined) {
    yield* enqueueEvent(createServiceActRequestedEvent({ actId: inserted.id, organizationId: inserted.organizationId }));
    return { ok: true, id: inserted.id } satisfies Admission;
  }
  const [replayed] = yield* drizzle.select().from(serviceAct)
    .where(and(eq(serviceAct.organizationId, caller.organizationId), eq(serviceAct.id, input.attemptId)));
  if (replayed !== undefined) return { ok: true, id: replayed.id } satisfies Admission;
  const [active] = yield* drizzle.select().from(serviceAct).where(and(
    eq(serviceAct.organizationId, caller.organizationId),
    eq(serviceAct.environmentId, input.environmentId),
    eq(serviceAct.service, input.service),
    inArray(serviceAct.state, ACTIVE_STATES),
  )).limit(1);
  const run = active === undefined ? null : operationOf(active);
  return {
    ok: false,
    refusal: {
      code: "conflict",
      message: run === null ? `${input.service} just finished another run. Try again.` : `${input.service} is busy: ${run.run} is still running.`,
      details: { run },
    },
  } satisfies Admission;
});

const readAct = Effect.fn("ServiceAct.read")(function* (actId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select().from(serviceAct).where(eq(serviceAct.id, actId));
  if (row === undefined) return yield* new NotFound({ message: "The service run was not found." });
  return row;
});

const owned = (request: ActRequest, runId: string) =>
  and(eq(serviceAct.id, request.actId), eq(serviceAct.organizationId, request.organizationId), eq(serviceAct.inngestRunId, runId));

export type ClaimedAct = { kind: "claimed"; verb: ServiceVerb; legs: ServiceActLeg[] } | { kind: "settled"; state: ServiceActState };

export const claimServiceAct = Effect.fn("ServiceAct.claim")(function* (request: ActRequest, runId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(serviceAct).set({ inngestRunId: runId }).where(and(
    eq(serviceAct.id, request.actId),
    eq(serviceAct.organizationId, request.organizationId),
    eq(serviceAct.state, "accepted"),
    or(isNull(serviceAct.inngestRunId), eq(serviceAct.inngestRunId, runId)),
  ));
  const row = yield* readAct(request.actId);
  if (row.organizationId !== request.organizationId) return yield* new NotFound({ message: "The service run was not found." });
  if (row.inngestRunId !== runId || (row.state !== "accepted" && row.state !== "running")) {
    return { kind: "settled", state: row.state } satisfies ClaimedAct;
  }
  if (row.state === "running") return { kind: "claimed", verb: row.verb, legs: row.legs } satisfies ClaimedAct;
  const session = yield* (yield* OrganizationRuntime).open(request.organizationId);
  if (session.status !== "connected") return yield* new ServiceActUnreachable({ cause: session.status });
  const before: ServiceActBefore = yield* session.connected.observeService(row.namespace, row.runtime);
  const legs = legsOf(before, row.only);
  const [claimed] = yield* drizzle.update(serviceAct).set({ state: "running", before, legs, startedAt: new Date() })
    .where(and(owned(request, runId), eq(serviceAct.state, "accepted"))).returning();
  if (claimed === undefined) return { kind: "settled", state: (yield* readAct(request.actId)).state } satisfies ClaimedAct;
  return { kind: "claimed", verb: claimed.verb, legs: claimed.legs } satisfies ClaimedAct;
}, Effect.scoped);

const writeLegs = (request: ActRequest, runId: string, legs: ServiceActLeg[]) =>
  Effect.flatMap(Database, ({ drizzle }) =>
    drizzle.update(serviceAct).set({ legs }).where(and(owned(request, runId), eq(serviceAct.state, "running"))).returning({ id: serviceAct.id }))
    .pipe(Effect.map((rows) => rows.length > 0));

export const actOnServer = Effect.fn("ServiceAct.leg")(function* (
  request: ActRequest,
  runId: string,
  server: string,
  side: "stop" | "start",
) {
  const row = yield* readAct(request.actId);
  if (row.state !== "running" || row.inngestRunId !== runId) return row.legs;
  const states = new Map((row.before?.copies ?? []).map((copy) => [copy.container, copy.state]));
  const legs = row.legs.map((leg) => ({ ...leg }));
  const write = (leg: ServiceActLeg, step: LegStep, reason: string | null = leg.reason) =>
    Effect.suspend(() => {
      leg[side] = step;
      leg.reason = reason;
      return writeLegs(request, runId, legs);
    });
  const session = yield* (yield* OrganizationRuntime).open(request.organizationId);
  for (const leg of legs) {
    if (leg.server !== server || leg[side] !== null && leg[side] !== "sent") continue;
    if (side === "start" && !mayStart(row.verb, leg)) continue;
    if (leg[side] === "sent") {
      if (!(yield* write(leg, "unknown", `${server} didn't answer.`))) return legs;
      continue;
    }
    if (alreadyThere(row.verb, side, states.get(leg.container))) {
      if (!(yield* write(leg, "already_there"))) return legs;
      continue;
    }
    if (session.status !== "connected") {
      if (!(yield* write(leg, "failed", `Cloud couldn't reach ${server}.`))) return legs;
      continue;
    }
    if (!(yield* write(leg, "sent"))) return legs;
    const call = side === "stop" ? session.connected.stopContainer : session.connected.startContainer;
    const [step, reason] = yield* call(leg.machine, leg.container).pipe(
      Effect.map(() => ["done", null] as const),
      Effect.catch((error) => Effect.succeed(rpcErrorCode(error) === "unavailable"
        ? ["unknown", `${server} didn't answer.`] as const
        : ["failed", `${server} couldn't ${side} it.`] as const)),
    );
    if (!(yield* write(leg, step, reason))) return legs;
  }
  return legs;
}, Effect.scoped);

export const recordServiceAct = Effect.fn("ServiceAct.record")(function* (request: ActRequest, runId: string) {
  const { drizzle } = yield* Database;
  const row = yield* readAct(request.actId);
  if (row.state !== "running" || row.inngestRunId !== runId || row.before === null) return { actId: row.id, state: row.state };
  const end = endOf(row.before, row.legs, row.only);
  const [written] = yield* drizzle.update(serviceAct).set({ ...end, endedAt: new Date() })
    .where(and(owned(request, runId), eq(serviceAct.state, "running"))).returning({ state: serviceAct.state });
  return { actId: row.id, state: written?.state ?? (yield* readAct(request.actId)).state };
});

const RUN_ENDS = {
  failure: { accepted: "The run never started.", running: "Cloud lost track of the run." },
  cancellation: { accepted: "The run was cancelled before it started.", running: "The run was cancelled while it ran." },
} as const;

export const closeServiceActRun = Effect.fn("ServiceAct.closeRun")(function* (
  runId: string,
  reason: keyof typeof RUN_ENDS,
  actId?: string,
) {
  const { drizzle } = yield* Database;
  const accepted = sql`${serviceAct.state} = 'accepted'`;
  const unbound = actId === undefined ? undefined : and(
    eq(serviceAct.id, actId),
    eq(serviceAct.state, "accepted"),
    or(isNull(serviceAct.inngestRunId), eq(serviceAct.inngestRunId, runId)),
  );
  const rows = yield* drizzle.update(serviceAct).set({
    state: sql<ServiceActState>`case when ${accepted} then 'failed' else 'unknown' end`,
    reason: sql<string>`case when ${accepted} then ${RUN_ENDS[reason].accepted} else ${RUN_ENDS[reason].running} end`,
    endedAt: new Date(),
  })
    .where(or(and(eq(serviceAct.inngestRunId, runId), inArray(serviceAct.state, ACTIVE_STATES)), unbound))
    .returning({ id: serviceAct.id });
  return rows.length;
});

export const closeStaleServiceActs = Effect.fn("ServiceAct.closeStale")(function* () {
  const { drizzle } = yield* Database;
  const now = Date.now();
  const unknown = yield* drizzle.update(serviceAct).set({ state: "unknown", reason: RUN_ENDS.failure.running, endedAt: new Date() })
    .where(and(eq(serviceAct.state, "running"), lt(serviceAct.startedAt, new Date(now - SERVICE_ACT_RUNNING_LIMIT_MS))))
    .returning({ id: serviceAct.id });
  const failed = yield* drizzle.update(serviceAct).set({ state: "failed", reason: RUN_ENDS.failure.accepted, endedAt: new Date() })
    .where(and(eq(serviceAct.state, "accepted"), lt(serviceAct.requestedAt, new Date(now - SERVICE_ACT_PENDING_LIMIT_MS))))
    .returning({ id: serviceAct.id });
  return { failed: failed.length, unknown: unknown.length };
});

export const serviceOperations = Effect.fn("ServiceAct.operations")(function* (
  organizationId: string,
  subject: { readonly environment: string; readonly service: string },
  attempt: string | null,
  limit: number,
) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(serviceAct).where(and(
    eq(serviceAct.organizationId, organizationId),
    eq(serviceAct.environmentId, subject.environment),
    eq(serviceAct.service, subject.service),
    attempt === null ? undefined : eq(serviceAct.id, attempt),
  )).orderBy(desc(serviceAct.requestedAt), desc(serviceAct.id)).limit(limit);
  return rows.map(operationOf);
});
