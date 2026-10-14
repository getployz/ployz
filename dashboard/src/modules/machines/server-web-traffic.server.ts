import "@tanstack/react-start/server-only";
import { and, desc, eq, inArray, isNull, lt, or, sql } from "drizzle-orm";
import { Data, Effect, Option, Schedule, Schema } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { createServerWebTrafficRequestedEvent, type ServerWebTrafficRequestedEventData } from "#/modules/inngest/events";
import {
  type LatestWebTraffic,
  type RequestedServerWebTraffic,
  type RequestServerWebTrafficInput,
  startFailure,
  WEB_TRAFFIC_PENDING_LIMIT_MS,
  WEB_TRAFFIC_RUNNING_LIMIT_MS,
  type WebTrafficState,
  type WebTrafficWant,
} from "#/modules/machines/server-web-traffic";
import { serverWebTrafficAttempt } from "#/modules/machines/tables";
import { enqueueEvent } from "#/modules/operations/outbox.server";
import { requireAccess } from "#/modules/organization/access.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSdkError, PloyzSession } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";

export class ServerWebTrafficUnreachable extends Data.TaggedError("ServerWebTrafficUnreachable")<{
  readonly operation: string;
  readonly cause: unknown;
}> {}

type WebTrafficAttemptRow = typeof serverWebTrafficAttempt.$inferSelect;
type WebTrafficRequest = ServerWebTrafficRequestedEventData;
/** What a run step answers with: the row's state once the step is done with it. */
export type WebTrafficRunReply = { readonly attemptId: string; readonly state: WebTrafficState };

const ACTIVE_STATES: readonly WebTrafficState[] = ["pending", "running"];
const MESSAGE_LIMIT = 1024;

/** A row as the page reads it. The check constraint keeps each state's evidence; a row without it is a defect. */
export function latestWebTrafficOf(row: WebTrafficAttemptRow): Effect.Effect<LatestWebTraffic> {
  const { id: attemptId, want } = row;
  const missing = (what: string) => Effect.die(new Error(`Web traffic change ${attemptId} is ${row.state} without its ${what}.`));
  switch (row.state) {
    case "pending":
      return Effect.succeed({ attemptId, want, state: "pending", requestedAt: row.requestedAt.toISOString() });
    case "running":
      return row.startedAt === null
        ? missing("start time")
        : Effect.succeed({ attemptId, want, state: "running", startedAt: row.startedAt.toISOString() });
    case "finished":
      return row.endedAt === null
        ? missing("end time")
        : Effect.succeed({ attemptId, want, state: "finished", endedAt: row.endedAt.toISOString() });
    case "failed":
      return row.endedAt === null || row.failure === null
        ? missing("failure")
        : Effect.succeed({ attemptId, want, state: "failed", endedAt: row.endedAt.toISOString(), failure: row.failure });
  }
}

const failed = (failure: string) => ({ state: "failed", failure: within(failure), endedAt: new Date() }) as const;
const finished = () => ({ state: "finished", failure: null, endedAt: new Date() }) as const;

const within = (text: string) => (text.length > MESSAGE_LIMIT ? `${text.slice(0, MESSAGE_LIMIT - 1)}…` : text);

/** Asks for the row's run in the caller's transaction; the outbox sends it once that commits. */
const enqueueChange = (row: WebTrafficAttemptRow) =>
  enqueueEvent(createServerWebTrafficRequestedEvent({ attemptId: row.id, organizationId: row.organizationId, machineId: row.machineId }));

/**
 * The switch on a Server page. The admitted change's row takes the id the tab minted and is written with its run's
 * event, so the same request again returns it. A flip while another change is active on the Server returns that one,
 * whichever way it goes: the switch waits for it.
 */
export const requestServerWebTraffic = Effect.fn("ServerWebTraffic.request")(function* (actor: Actor, input: RequestServerWebTrafficInput) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const organizationId = organization.id;
  const database = yield* Database;
  const sameOrActive = Effect.fn(function* () {
    const { drizzle } = yield* Database;
    const [same] = yield* drizzle.select().from(serverWebTrafficAttempt).where(eq(serverWebTrafficAttempt.id, input.requestId));
    if (same !== undefined) {
      if (same.organizationId !== organizationId || same.machineId !== input.machineId) {
        return yield* new Conflict({ message: "The web traffic request names another server." });
      }
      return { change: yield* latestWebTrafficOf(same) } satisfies RequestedServerWebTraffic;
    }
    const [active] = yield* drizzle.select().from(serverWebTrafficAttempt).where(and(
      eq(serverWebTrafficAttempt.organizationId, organizationId),
      eq(serverWebTrafficAttempt.machineId, input.machineId),
      inArray(serverWebTrafficAttempt.state, ACTIVE_STATES),
    )).orderBy(desc(serverWebTrafficAttempt.requestedAt)).limit(1);
    return active === undefined ? null : { change: yield* latestWebTrafficOf(active) } satisfies RequestedServerWebTraffic;
  });
  const existing = yield* sameOrActive();
  if (existing !== null) return existing;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [inserted] = yield* drizzle.insert(serverWebTrafficAttempt).values({
      id: input.requestId,
      organizationId,
      machineId: input.machineId,
      requestedByUserId: actor.userId,
      want: input.want,
    }).onConflictDoNothing().returning();
    if (inserted !== undefined) {
      yield* enqueueChange(inserted);
      return { change: yield* latestWebTrafficOf(inserted) } satisfies RequestedServerWebTraffic;
    }
    const raced = yield* sameOrActive();
    if (raced !== null) return raced;
    return yield* new Conflict({ userFacing: true, message: "A web traffic change on this server just ended. Try again." });
  }));
});

/** Each Server's latest web traffic change in the Organization, keyed by Machine ID. */
export const listLatestServerWebTraffic = Effect.fn("ServerWebTraffic.listLatest")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.selectDistinctOn([serverWebTrafficAttempt.machineId]).from(serverWebTrafficAttempt)
    .where(eq(serverWebTrafficAttempt.organizationId, organization.id))
    .orderBy(serverWebTrafficAttempt.machineId, desc(serverWebTrafficAttempt.requestedAt), desc(serverWebTrafficAttempt.id));
  const latest = yield* Effect.forEach(rows, (row) => Effect.map(latestWebTrafficOf(row), (change) => [row.machineId, change] as const));
  return { servers: Object.fromEntries(latest) };
});

/**
 * Bind the run to its pending row, so only it may claim and end the row. The event must name the row's Organization
 * and Server. A row that is no longer pending (bound by another run, or already ended) is settled: the run does nothing.
 */
export const bindWebTrafficRun = Effect.fn("ServerWebTraffic.bind")(function* (request: WebTrafficRequest, runId: string) {
  const { drizzle } = yield* Database;
  const [bound] = yield* drizzle.update(serverWebTrafficAttempt).set({ inngestRunId: runId }).where(and(
    eq(serverWebTrafficAttempt.id, request.attemptId),
    eq(serverWebTrafficAttempt.organizationId, request.organizationId),
    eq(serverWebTrafficAttempt.machineId, request.machineId),
    eq(serverWebTrafficAttempt.state, "pending"),
    or(isNull(serverWebTrafficAttempt.inngestRunId), eq(serverWebTrafficAttempt.inngestRunId, runId)),
  )).returning({ id: serverWebTrafficAttempt.id });
  if (bound !== undefined) return { kind: "bound" } as const;
  const [row] = yield* drizzle.select({
    state: serverWebTrafficAttempt.state, organizationId: serverWebTrafficAttempt.organizationId, machineId: serverWebTrafficAttempt.machineId,
  }).from(serverWebTrafficAttempt).where(eq(serverWebTrafficAttempt.id, request.attemptId));
  if (row === undefined || row.organizationId !== request.organizationId || row.machineId !== request.machineId) {
    return yield* new NotFound({ message: "The web traffic request was not found." });
  }
  return { kind: "settled", state: row.state } as const;
});

const openSession = Effect.fn("ServerWebTraffic.openSession")(function* (organizationId: string) {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status !== "connected") {
    return yield* new ServerWebTrafficUnreachable({ operation: "open organization runtime", cause: session.status });
  }
  return session.connected;
});

const decodeMessage = Schema.decodeUnknownOption(Schema.Struct({ message: Schema.String }));

/** What the Engine said when it refused. */
function refusalWords(error: PloyzSdkError) {
  const rpc = decodeMessage("cause" in error ? error.cause : undefined);
  const message = Option.isSome(rpc) ? rpc.value.message : error.message;
  return message.trim().length > 0 ? message.trim() : "Your servers didn't answer.";
}

/** How long putting the role back may take once a change failed. */
const REVERT_LIMIT_MS = 30_000;

/**
 * Apply `want`: the role first, then the web server. On, the Cluster advertises the Server before its web server
 * starts; off, nothing new is sent here before it stops. An on that doesn't finish, refused, thrown, or out of time,
 * turns the role back off, so the Server is never advertised with nothing listening. An off that doesn't finish keeps
 * the role off: its web server may already be gone.
 */
const applyWant = Effect.fn("ServerWebTraffic.apply")(function* (session: PloyzSession, request: WebTrafficRequest, want: WebTrafficWant) {
  const machine = request.machineId;
  const role = want === "on";
  const end = yield* Effect.gen(function* () {
    yield* session.updateMachine(machine, { accepts_ingress: role });
    if (!role) {
      yield* session.stopIngress(machine);
      return finished();
    }
    const failure = startFailure(yield* session.startIngress(machine), machine);
    return failure === null ? finished() : failed(failure);
  }).pipe(
    Effect.catch((error) => Effect.succeed(failed(refusalWords(error)))),
    Effect.catchDefect((defect) =>
      Effect.as(Effect.logError("Web traffic change died", defect), failed("Cloud lost track of the change. Try again."))),
    Effect.timeoutOrElse({
      duration: WEB_TRAFFIC_RUNNING_LIMIT_MS,
      orElse: () => Effect.succeed(failed("Your servers didn't answer in time. Try again.")),
    }),
  );
  if (end.state === "failed" && role) {
    yield* session.updateMachine(machine, { accepts_ingress: false }).pipe(Effect.timeout(REVERT_LIMIT_MS),
      Effect.ignoreCause({ log: true, message: "Web traffic role not put back" }));
  }
  return end;
});

const stateOf = Effect.fn("ServerWebTraffic.stateOf")(function* (attemptId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ state: serverWebTrafficAttempt.state }).from(serverWebTrafficAttempt)
    .where(eq(serverWebTrafficAttempt.id, attemptId));
  if (row === undefined) return yield* new NotFound({ message: "The web traffic request was not found." });
  return { attemptId, state: row.state } satisfies WebTrafficRunReply;
});

/**
 * Ask the Engine once. The row is claimed (`running`) right before the call and ended with the Engine's answer right
 * after, in one step: a retry that finds the row running knows the Engine may have been asked, closes it failed and
 * never asks again; one that finds it ended returns what's there. The page reads the Server's observed state beside
 * the row (see `webTrafficView`).
 */
export const executeWebTrafficOnce = Effect.fn("ServerWebTraffic.execute")(function* (request: WebTrafficRequest, runId: string) {
  const { drizzle } = yield* Database;
  const owned = and(eq(serverWebTrafficAttempt.id, request.attemptId), eq(serverWebTrafficAttempt.inngestRunId, runId));
  const current = yield* stateOf(request.attemptId);
  if (current.state === "running") {
    yield* drizzle.update(serverWebTrafficAttempt)
      .set(failed("Cloud lost track of the change. Try again."))
      .where(and(owned, eq(serverWebTrafficAttempt.state, "running")));
    return yield* stateOf(request.attemptId);
  }
  if (current.state !== "pending") return current;
  const [row] = yield* drizzle.select({ want: serverWebTrafficAttempt.want }).from(serverWebTrafficAttempt)
    .where(eq(serverWebTrafficAttempt.id, request.attemptId));
  if (row === undefined) return yield* new NotFound({ message: "The web traffic request was not found." });
  const session = yield* openSession(request.organizationId);
  const [claimed] = yield* drizzle.update(serverWebTrafficAttempt).set({ state: "running", startedAt: new Date() })
    .where(and(owned, eq(serverWebTrafficAttempt.state, "pending"))).returning({ id: serverWebTrafficAttempt.id });
  if (claimed === undefined) return yield* stateOf(request.attemptId);
  const end = yield* applyWant(session, request, row.want);
  // The answer is in hand: the write is retried here rather than by asking the Engine again.
  const [written] = yield* drizzle.update(serverWebTrafficAttempt).set(end)
    .where(and(owned, eq(serverWebTrafficAttempt.state, "running"))).returning({ state: serverWebTrafficAttempt.state })
    .pipe(Effect.retry({ times: 4, schedule: Schedule.exponential("500 millis") }));
  return written === undefined ? yield* stateOf(request.attemptId) : { attemptId: request.attemptId, state: written.state };
}, Effect.scoped);

/** How a run that ended for good closes its row: by whether the row ever asked the Engine. */
const RUN_ENDS = {
  failure: { pending: "The change never started. Try again.", running: "Cloud lost track of the change. Try again." },
  cancellation: { pending: "The change was cancelled before it started.", running: "The change was cancelled while it ran." },
} as const satisfies Record<string, Record<"pending" | "running", string>>;

/**
 * A run that failed for good or was cancelled must not leave its row active: it ends failed, with words that say whether
 * the Engine was ever asked. A run cancelled before it bound its row names the row by `attemptId` from its event. How
 * many rows this call ended.
 */
export const closeWebTrafficRun = Effect.fn("ServerWebTraffic.closeRun")(function* (
  runId: string,
  reason: keyof typeof RUN_ENDS,
  attemptId?: string,
) {
  const { drizzle } = yield* Database;
  const { pending, running } = RUN_ENDS[reason];
  const unbound = attemptId === undefined ? undefined : and(
    eq(serverWebTrafficAttempt.id, attemptId),
    eq(serverWebTrafficAttempt.state, "pending"),
    or(isNull(serverWebTrafficAttempt.inngestRunId), eq(serverWebTrafficAttempt.inngestRunId, runId)),
  );
  const rows = yield* drizzle.update(serverWebTrafficAttempt).set({
    state: "failed",
    failure: sql<string>`case when ${serverWebTrafficAttempt.state} = 'pending' then ${pending} else ${running} end`,
    endedAt: new Date(),
  })
    .where(or(and(eq(serverWebTrafficAttempt.inngestRunId, runId), inArray(serverWebTrafficAttempt.state, ACTIVE_STATES)), unbound))
    .returning({ id: serverWebTrafficAttempt.id });
  return rows.length;
});

/**
 * The hourly sweep: a request no run picked up in time never starts; a change running past the Engine's limit lost
 * its run. Changes in an Organization run one at a time, so a request waits its turn while one runs there or an
 * earlier request is ahead of it, and its time runs from when the change before it ended.
 */
export const closeStaleWebTraffic = Effect.fn("ServerWebTraffic.closeStale")(function* () {
  const { drizzle } = yield* Database;
  const now = Date.now();
  const lost = yield* drizzle.update(serverWebTrafficAttempt)
    .set(failed(RUN_ENDS.failure.running))
    .where(and(
      eq(serverWebTrafficAttempt.state, "running"),
      lt(serverWebTrafficAttempt.startedAt, new Date(now - 2 * WEB_TRAFFIC_RUNNING_LIMIT_MS)),
    ))
    .returning({ id: serverWebTrafficAttempt.id });
  const unstarted = yield* drizzle.update(serverWebTrafficAttempt)
    .set(failed(RUN_ENDS.failure.pending))
    .where(and(
      eq(serverWebTrafficAttempt.state, "pending"),
      // Spelled out: an update renders its columns unqualified, which inside a subquery would name the alias's.
      sql`not exists (
        select 1 from server_web_traffic_attempt as ahead
        where ahead.organization_id = server_web_traffic_attempt.organization_id
          and (ahead.state = 'running' or (ahead.state = 'pending' and ahead.requested_at < server_web_traffic_attempt.requested_at))
      )`,
      sql`greatest(server_web_traffic_attempt.requested_at, (
        select max(before.ended_at) from server_web_traffic_attempt as before
        where before.organization_id = server_web_traffic_attempt.organization_id
          and before.requested_at < server_web_traffic_attempt.requested_at
      )) < ${new Date(now - WEB_TRAFFIC_PENDING_LIMIT_MS)}`,
    ))
    .returning({ id: serverWebTrafficAttempt.id });
  return { unstarted: unstarted.length, lost: lost.length };
});
