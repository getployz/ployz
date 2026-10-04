import "@tanstack/react-start/server-only";
import type { DrainScope } from "@ployz/sdk";
import { and, desc, eq, inArray, isNull, lt, or, sql } from "drizzle-orm";
import { Data, Effect, Option, Schedule, Schema } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createServerDrainRequestedEvent, type ServerDrainRequestedEventData } from "#/modules/inngest/events";
import { ownedNamespaces } from "#/modules/machines/namespace-cleanup.server";
import {
  DRAIN_PENDING_LIMIT_MS,
  DRAIN_RUNNING_LIMIT_MS,
  type DrainFailureCode,
  type DrainState,
  type LatestDrain,
  type RequestServerDrainInput,
} from "#/modules/machines/server-drain";
import { serverDrainAttempt } from "#/modules/machines/tables";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSdkError } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";

export class ServerDrainUnreachable extends Data.TaggedError("ServerDrainUnreachable")<{
  readonly operation: string;
  readonly cause: unknown;
}> {}

type DrainRow = typeof serverDrainAttempt.$inferSelect;
type DrainRequest = ServerDrainRequestedEventData;
/** What a run step answers with: the row's state once the step is done with it. */
export type DrainRunReply = { readonly attemptId: string; readonly state: DrainState };

const ACTIVE_STATES: readonly DrainState[] = ["pending", "running"];
const MESSAGE_LIMIT = 1024;

/** A row as the page reads it. The check constraint keeps each state's evidence; a row without it reads as lost. */
export function latestDrainOf(row: DrainRow): LatestDrain {
  const attemptId = row.id;
  const endedAt = (row.endedAt ?? row.requestedAt).toISOString();
  const lost = { attemptId, state: "unknown", endedAt, failureCode: "lost", failureMessage: "Cloud lost track of this drain." } as const;
  switch (row.state) {
    case "pending":
      return { attemptId, state: "pending", requestedAt: row.requestedAt.toISOString() };
    case "running":
      return { attemptId, state: "running", startedAt: (row.startedAt ?? row.requestedAt).toISOString() };
    case "finished":
      return row.report === null ? lost : { attemptId, state: "finished", endedAt, report: row.report };
    case "failed":
    case "cancelled":
    case "unknown":
      return { attemptId, state: row.state, endedAt, failureCode: row.failureCode ?? "lost", failureMessage: row.failureMessage ?? lost.failureMessage };
  }
}

const closed = (state: Exclude<DrainState, "pending" | "running" | "finished">, failureCode: DrainFailureCode, failureMessage: string) =>
  ({ state, endedAt: new Date(), failureCode, failureMessage }) as const;

/**
 * Drain on a Server page: any member may. Writes the Drain's row under the id the tab minted, then asks for its run.
 * The same request again returns the same row; a click while another Drain is active on the Server returns that one.
 * A request whose run could not be asked for ends `failed` at once, so no tab waits on it.
 */
export const requestServerDrain = Effect.fn("ServerDrain.request")(function* (actor: Actor, input: RequestServerDrainInput) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const { drizzle } = yield* Database;
  const [inserted] = yield* drizzle.insert(serverDrainAttempt).values({
    id: input.requestId,
    organizationId: organization.id,
    machineId: input.machineId,
    requestedByUserId: actor.userId,
  }).onConflictDoNothing().returning();
  if (inserted === undefined) {
    const [same] = yield* drizzle.select().from(serverDrainAttempt).where(eq(serverDrainAttempt.id, input.requestId));
    if (same !== undefined) {
      if (same.organizationId !== organization.id || same.machineId !== input.machineId) {
        return yield* new Conflict({ message: "The drain request names another server." });
      }
      return latestDrainOf(same);
    }
    const [active] = yield* drizzle.select().from(serverDrainAttempt).where(and(
      eq(serverDrainAttempt.organizationId, organization.id),
      eq(serverDrainAttempt.machineId, input.machineId),
      inArray(serverDrainAttempt.state, ACTIVE_STATES),
    )).orderBy(desc(serverDrainAttempt.requestedAt)).limit(1);
    if (active !== undefined) return latestDrainOf(active);
    return yield* new Conflict({ userFacing: true, message: "A drain on this server just ended. Try again." });
  }
  yield* sendInngestEvent(createServerDrainRequestedEvent({
    attemptId: inserted.id,
    organizationId: organization.id,
    machineId: input.machineId,
  })).pipe(Effect.tapError(() =>
    // Only a row no run has bound: a run that got the event anyway keeps it.
    drizzle.update(serverDrainAttempt)
      .set(closed("failed", "dispatch_failed", "The drain could not be queued."))
      .where(and(eq(serverDrainAttempt.id, inserted.id), eq(serverDrainAttempt.state, "pending"), isNull(serverDrainAttempt.inngestRunId)))));
  return latestDrainOf(inserted);
});

/** Each Server's latest Drain in the Organization, keyed by Machine ID. */
export const listLatestServerDrains = Effect.fn("ServerDrain.listLatest")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string },
) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.selectDistinctOn([serverDrainAttempt.machineId]).from(serverDrainAttempt)
    .where(eq(serverDrainAttempt.organizationId, organization.id))
    .orderBy(serverDrainAttempt.machineId, desc(serverDrainAttempt.requestedAt), desc(serverDrainAttempt.id));
  return { servers: Object.fromEntries(rows.map((row) => [row.machineId, latestDrainOf(row)])) };
});

/** Whether a Cloud Drain is pending or running on the Server: turning services back on would undo it. */
export const drainActiveOn = Effect.fn("ServerDrain.activeOn")(function* (organizationId: string, machineId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ id: serverDrainAttempt.id }).from(serverDrainAttempt).where(and(
    eq(serverDrainAttempt.organizationId, organizationId),
    eq(serverDrainAttempt.machineId, machineId),
    inArray(serverDrainAttempt.state, ACTIVE_STATES),
  )).limit(1);
  return row !== undefined;
});

/**
 * Bind the run to its pending row, so only it may claim and end the row. The event must name the row's Organization
 * and Server. A row that is no longer pending (bound by another run, or already ended) is settled: the run does nothing.
 */
export const bindDrainRun = Effect.fn("ServerDrain.bind")(function* (request: DrainRequest, runId: string) {
  const { drizzle } = yield* Database;
  const [bound] = yield* drizzle.update(serverDrainAttempt).set({ inngestRunId: runId }).where(and(
    eq(serverDrainAttempt.id, request.attemptId),
    eq(serverDrainAttempt.organizationId, request.organizationId),
    eq(serverDrainAttempt.machineId, request.machineId),
    eq(serverDrainAttempt.state, "pending"),
    or(isNull(serverDrainAttempt.inngestRunId), eq(serverDrainAttempt.inngestRunId, runId)),
  )).returning({ id: serverDrainAttempt.id });
  if (bound !== undefined) return { kind: "bound" } as const;
  const [row] = yield* drizzle.select({
    state: serverDrainAttempt.state, organizationId: serverDrainAttempt.organizationId, machineId: serverDrainAttempt.machineId,
  }).from(serverDrainAttempt).where(eq(serverDrainAttempt.id, request.attemptId));
  if (row === undefined || row.organizationId !== request.organizationId || row.machineId !== request.machineId) {
    return yield* new NotFound({ message: "The drain request was not found." });
  }
  return { kind: "settled", state: row.state } as const;
});

/** What the Drain selects: the Namespaces the Organization's Environments own. Containers no Project owns stay. */
export const prepareDrain = Effect.fn("ServerDrain.prepare")(function* (organizationId: string) {
  const namespaces = yield* ownedNamespaces(organizationId);
  return { scope: "owned", namespaces } satisfies DrainScope;
});

const openSession = Effect.fn("ServerDrain.openSession")(function* (organizationId: string) {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status !== "connected") {
    return yield* new ServerDrainUnreachable({ operation: "open organization runtime", cause: session.status });
  }
  return session.connected;
});

const decodeMessage = Schema.decodeUnknownOption(Schema.Struct({ message: Schema.String }));

/** What the Engine said when it refused, within the row's limit. */
function refusalMessage(error: PloyzSdkError) {
  const rpc = decodeMessage("cause" in error ? error.cause : undefined);
  const message = Option.isSome(rpc) ? rpc.value.message : error.message;
  const text = message.trim().length > 0 ? message.trim() : "The drain was refused.";
  return text.length > MESSAGE_LIMIT ? `${text.slice(0, MESSAGE_LIMIT - 1)}…` : text;
}

const stateOf = Effect.fn("ServerDrain.stateOf")(function* (attemptId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ state: serverDrainAttempt.state }).from(serverDrainAttempt).where(eq(serverDrainAttempt.id, attemptId));
  if (row === undefined) return yield* new NotFound({ message: "The drain request was not found." });
  return { attemptId, state: row.state } satisfies DrainRunReply;
});

/**
 * Ask the Engine once. The row is claimed (`running`) right before the call and ended with the Engine's answer right
 * after, in one step: a retry that finds the row running knows the Engine may have been asked, closes it as unknown
 * and never asks again; one that finds it ended returns what's there. An error from the Engine means nothing moved
 * (its preflight refused), so the row ends `failed`. A closer that got there first (a cancellation) keeps its word:
 * the report is never written over an ended row.
 */
export const executeDrainOnce = Effect.fn("ServerDrain.execute")(function* (request: DrainRequest, runId: string, scope: DrainScope) {
  const { drizzle } = yield* Database;
  const owned = and(eq(serverDrainAttempt.id, request.attemptId), eq(serverDrainAttempt.inngestRunId, runId));
  const current = yield* stateOf(request.attemptId);
  if (current.state === "running") {
    yield* drizzle.update(serverDrainAttempt)
      .set(closed("unknown", "lost", "Cloud lost track of this drain while it ran."))
      .where(and(owned, eq(serverDrainAttempt.state, "running")));
    return yield* stateOf(request.attemptId);
  }
  if (current.state !== "pending") return current;
  // Before the claim: a cluster Cloud can't reach leaves the row pending for the retry.
  const session = yield* openSession(request.organizationId);
  const [claimed] = yield* drizzle.update(serverDrainAttempt).set({ state: "running", startedAt: new Date() })
    .where(and(owned, eq(serverDrainAttempt.state, "pending"))).returning({ id: serverDrainAttempt.id });
  if (claimed === undefined) return yield* stateOf(request.attemptId);
  const end = yield* session.drainMachine(request.machineId, scope).pipe(
    Effect.map((report) => ({ state: "finished", report, endedAt: new Date() }) as const),
    Effect.catch((error) => Effect.succeed(closed("failed", "refused", refusalMessage(error)))),
  );
  // The answer is in hand: the write is retried here rather than by asking the Engine again.
  const [ended] = yield* drizzle.update(serverDrainAttempt).set(end)
    .where(and(owned, eq(serverDrainAttempt.state, "running"))).returning({ state: serverDrainAttempt.state })
    .pipe(Effect.retry({ times: 4, schedule: Schedule.exponential("500 millis") }));
  return ended === undefined ? yield* stateOf(request.attemptId) : { attemptId: request.attemptId, state: ended.state };
}, Effect.scoped);

/**
 * A run that failed for good or was cancelled must not leave its row active. A failed run's pending row never asked
 * the Engine (`failed`); its running row may have (`unknown`). Cancellation ends either as `cancelled`: Cloud can't
 * undo what the Engine did, and Running here shows it. How many rows this call ended.
 */
export const closeDrainRun = Effect.fn("ServerDrain.closeRun")(function* (runId: string, reason: "failure" | "cancellation") {
  const { drizzle } = yield* Database;
  const pending = sql`${serverDrainAttempt.state} = 'pending'`;
  const rows = yield* drizzle.update(serverDrainAttempt).set(reason === "cancellation"
    ? closed("cancelled", "cancelled", "The drain run was cancelled.")
    : {
      state: sql<DrainState>`case when ${pending} then 'failed' else 'unknown' end`,
      failureCode: sql<DrainFailureCode>`case when ${pending} then 'workflow_failed' else 'lost' end`,
      failureMessage: sql<string>`case when ${pending} then 'The drain run failed before it started.' else 'The drain run failed while it ran.' end`,
      endedAt: new Date(),
    })
    .where(and(eq(serverDrainAttempt.inngestRunId, runId), inArray(serverDrainAttempt.state, ACTIVE_STATES)))
    .returning({ id: serverDrainAttempt.id });
  return rows.length;
});

/**
 * The hourly sweep: a request no run picked up in time never starts; a Drain running for a day lost its run. A request
 * queued behind another Server's running Drain in its Organization is waiting its turn, not lost: it stays.
 */
export const closeStaleDrains = Effect.fn("ServerDrain.closeStale")(function* () {
  const { drizzle } = yield* Database;
  const now = Date.now();
  // First: a request behind a Drain that lost its run is not waiting on anything.
  const unknown = yield* drizzle.update(serverDrainAttempt)
    .set(closed("unknown", "lost", "The drain ran for a day without an answer."))
    .where(and(eq(serverDrainAttempt.state, "running"), lt(serverDrainAttempt.startedAt, new Date(now - DRAIN_RUNNING_LIMIT_MS))))
    .returning({ id: serverDrainAttempt.id });
  const failed = yield* drizzle.update(serverDrainAttempt)
    .set(closed("failed", "never_started", "No run picked the drain up in time."))
    .where(and(
      eq(serverDrainAttempt.state, "pending"),
      lt(serverDrainAttempt.requestedAt, new Date(now - DRAIN_PENDING_LIMIT_MS)),
      // Spelled out: an update renders its columns unqualified, which inside the subquery would name `running`'s.
      sql`not exists (
        select 1 from server_drain_attempt as running
        where running.organization_id = server_drain_attempt.organization_id and running.state = 'running'
      )`,
    ))
    .returning({ id: serverDrainAttempt.id });
  return { failed: failed.length, unknown: unknown.length };
});
