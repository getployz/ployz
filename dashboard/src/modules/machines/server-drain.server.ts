import "@tanstack/react-start/server-only";
import type { DrainReport, DrainScope, QualifiedService } from "@ployz/sdk";
import { and, desc, eq, inArray, isNull, lt, or, sql } from "drizzle-orm";
import { Data, Effect, Option, Schedule, Schema } from "effect";
import type { MachineId } from "#/db/tables";
import { listNames } from "#/lib/plural";
import { gateOperation, operationDigest } from "#/modules/approvals/approvals.server";
import type { Actor, Caller } from "#/modules/identity/actor";
import { createServerDrainRequestedEvent, type ServerDrainRequestedEventData } from "#/modules/inngest/events";
import { ownedNamespaces } from "#/modules/machines/namespace-cleanup.server";
import { enqueueEvent } from "#/modules/operations/outbox.server";
import {
  DRAIN_END_CODES,
  DRAIN_PENDING_LIMIT_MS,
  DRAIN_RUNNING_LIMIT_MS,
  type DrainEndCode,
  type DrainEndCodeWithoutMessage,
  type DrainState,
  type LatestDrain,
  type RequestedServerDrain,
  type RequestServerDrainInput,
} from "#/modules/machines/server-drain";
import { planDrain } from "#/modules/machines/server-operations.server";
import { splitQualifiedService } from "#/modules/machines/server-services";
import { serverDrainAttempt } from "#/modules/machines/tables";
import { serversLackingEnsure } from "#/modules/runtime/daemon-compat.server";
import { requireAccess } from "#/modules/organization/access.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSdkError } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";

export class ServerDrainUnreachable extends Data.TaggedError("ServerDrainUnreachable")<{
  readonly operation: string;
  readonly cause: unknown;
}> {}

type DrainAttemptRow = typeof serverDrainAttempt.$inferSelect;
type DrainRequest = ServerDrainRequestedEventData;
/** What a run step answers with: the row's state once the step is done with it. */
export type DrainRunReply = { readonly attemptId: string; readonly state: DrainState };

const ACTIVE_STATES: readonly DrainState[] = ["pending", "running"];
const MESSAGE_LIMIT = 1024;

/** A row as the page reads it. The check constraint keeps each state's evidence; a row without it is a defect. */
export function latestDrainOf(row: DrainAttemptRow): Effect.Effect<LatestDrain> {
  const attemptId = row.id;
  const missing = (what: string) => Effect.die(new Error(`Drain ${attemptId} is ${row.state} without its ${what}.`));
  switch (row.state) {
    case "pending":
      return Effect.succeed({ attemptId, state: "pending", requestedAt: row.requestedAt.toISOString() });
    case "running":
      return row.startedAt === null
        ? missing("start time")
        : Effect.succeed({ attemptId, state: "running", startedAt: row.startedAt.toISOString() });
    case "finished":
      return row.endedAt === null || row.report === null
        ? missing("report")
        : Effect.succeed({ attemptId, state: "finished", endedAt: row.endedAt.toISOString(), report: row.report });
    case "failed":
    case "cancelled":
    case "unknown": {
      const { endedAt, endCode, refusalMessage } = row;
      if (endedAt === null || endCode === null || DRAIN_END_CODES[endCode] !== row.state) return missing("end");
      const end = { attemptId, endedAt: endedAt.toISOString() };
      // One arm per code so the compiler pairs each code with its own state; merged arms would need a cast.
      switch (endCode) {
        case "refused":
          return refusalMessage === null
            ? missing("refusal")
            : Effect.succeed({ ...end, state: DRAIN_END_CODES[endCode], endCode, refusalMessage });
        case "not_started":
          return Effect.succeed({ ...end, state: DRAIN_END_CODES[endCode], endCode });
        case "cancelled":
          return Effect.succeed({ ...end, state: DRAIN_END_CODES[endCode], endCode });
        case "interrupted":
          return Effect.succeed({ ...end, state: DRAIN_END_CODES[endCode], endCode });
        case "lost":
          return Effect.succeed({ ...end, state: DRAIN_END_CODES[endCode], endCode });
      }
    }
  }
}

/** The columns that end a row under `code`; its state follows from the code. */
const ended = (code: DrainEndCodeWithoutMessage) =>
  ({ state: DRAIN_END_CODES[code], endCode: code, refusalMessage: null, endedAt: new Date() }) as const;
const refused = (message: string) =>
  ({ state: DRAIN_END_CODES.refused, endCode: "refused", refusalMessage: message, endedAt: new Date() }) as const;

/** Asks for the row's run in the caller's transaction; the outbox sends it once that commits. */
const enqueueDrain = (row: DrainAttemptRow) =>
  enqueueEvent(createServerDrainRequestedEvent({ attemptId: row.id, organizationId: row.organizationId, machineId: row.machineId }));

/**
 * Drain on a Server page: any member may, and confirming it is their approval, so a plan that now stops a Service the
 * dialog didn't show as stopping is refused. The admitted Drain's row takes the id
 * the tab minted and is written with its run's event, so the same request again returns it. A click while another
 * Drain is active on the Server returns that one.
 */
export const requestServerDrain = Effect.fn("ServerDrain.request")(function* (caller: Caller, input: RequestServerDrainInput) {
  const organizationId = caller.organization.id;
  const database = yield* Database;
  const sameOrActive = Effect.fn(function* () {
    const { drizzle } = yield* Database;
    const [same] = yield* drizzle.select().from(serverDrainAttempt).where(eq(serverDrainAttempt.id, input.requestId));
    if (same !== undefined) {
      if (same.organizationId !== organizationId || same.machineId !== input.machineId) {
        return yield* new Conflict({ message: "The drain request names another server." });
      }
      return { drain: yield* latestDrainOf(same) } satisfies RequestedServerDrain;
    }
    const [active] = yield* drizzle.select().from(serverDrainAttempt).where(and(
      eq(serverDrainAttempt.organizationId, organizationId),
      eq(serverDrainAttempt.machineId, input.machineId),
      inArray(serverDrainAttempt.state, ACTIVE_STATES),
    )).orderBy(desc(serverDrainAttempt.requestedAt)).limit(1);
    return active === undefined ? null : { drain: yield* latestDrainOf(active) } satisfies RequestedServerDrain;
  });
  const existing = yield* sameOrActive();
  if (existing !== null) return existing;
  const plan = yield* planDrain(organizationId, input.machineId);
  if (plan === null) return yield* new NotFound({ message: "The server was not found." });
  const unseen = plan.effects.flatMap(({ kind, name }) =>
    kind === "removes_service" && name !== undefined && !input.stops.includes(name) ? [splitQualifiedService(name).name] : []);
  if (unseen.length > 0) {
    const names = listNames(unseen);
    return yield* new Conflict({ userFacing: true, message: `Draining now stops ${names}. Review the drain again.` });
  }
  const gated = yield* gateOperation(caller, null, plan);
  if (!gated.ok) return yield* new Conflict({ userFacing: true, message: gated.refusal.message });
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [inserted] = yield* drizzle.insert(serverDrainAttempt).values({
      id: input.requestId,
      organizationId,
      machineId: input.machineId,
      requestedByUserId: caller.userId,
      targets: [...plan.targets],
      approvalId: gated.approvalId,
      planDigest: operationDigest(plan),
    }).onConflictDoNothing().returning();
    if (inserted !== undefined) {
      yield* enqueueDrain(inserted);
      return { drain: yield* latestDrainOf(inserted) } satisfies RequestedServerDrain;
    }
    const raced = yield* sameOrActive();
    if (raced !== null) return raced;
    return yield* new Conflict({ userFacing: true, message: "A drain on this server just ended. Try again." });
  }));
});

/**
 * A Drain the signed-in CLI asked for, of exactly the services it previewed, under the attempt its request admits.
 * Answers the Drain that consumed the same approval, or the one already active on the Server, instead of a second.
 */
export const requestCliServerDrain = Effect.fn("ServerDrain.requestCli")(function* (
  caller: { readonly organizationId: string; readonly userId: string },
  input: {
    readonly attemptId: string;
    readonly machineId: MachineId;
    readonly targets: readonly QualifiedService[];
    readonly approvalId: string | null;
    readonly planDigest: string;
  },
) {
  const { drizzle } = yield* Database;
  const [inserted] = yield* drizzle.insert(serverDrainAttempt).values({
    id: input.attemptId,
    organizationId: caller.organizationId,
    machineId: input.machineId,
    requestedByUserId: caller.userId,
    targets: [...input.targets],
    approvalId: input.approvalId,
    planDigest: input.planDigest,
  }).onConflictDoNothing().returning();
  if (inserted !== undefined) {
    yield* enqueueDrain(inserted);
    return inserted.id;
  }
  const consumed = input.approvalId === null ? [] : yield* drizzle.select().from(serverDrainAttempt).where(and(
    eq(serverDrainAttempt.organizationId, caller.organizationId),
    eq(serverDrainAttempt.approvalId, input.approvalId),
  ));
  const [active] = consumed.length > 0 ? consumed : yield* drizzle.select().from(serverDrainAttempt).where(and(
    eq(serverDrainAttempt.organizationId, caller.organizationId),
    eq(serverDrainAttempt.machineId, input.machineId),
    inArray(serverDrainAttempt.state, ACTIVE_STATES),
  )).limit(1);
  if (active === undefined) return yield* new Conflict({ userFacing: true, message: "A drain on this server just ended. Try again." });
  return active.id;
});

const CLI_END_MESSAGES = {
  not_started: "The drain never started.",
  cancelled: "The drain was cancelled before it started.",
  interrupted: "The drain was cancelled while it ran; some services may have moved.",
  lost: "Cloud lost track of the drain; some services may have moved.",
} as const satisfies Record<DrainEndCodeWithoutMessage, string>;

export type CliServerDrain =
  | { readonly state: "pending" | "running" }
  | { readonly state: "finished"; readonly report: DrainReport }
  | { readonly state: "ended"; readonly code: DrainEndCode; readonly message: string };

/** One Drain in the caller's Organization as the CLI polls it. */
export const readCliServerDrain = Effect.fn("ServerDrain.readCli")(function* (organizationId: string, attemptId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select().from(serverDrainAttempt)
    .where(and(eq(serverDrainAttempt.organizationId, organizationId), eq(serverDrainAttempt.id, attemptId)));
  if (row === undefined) return null;
  const drain = yield* latestDrainOf(row);
  switch (drain.state) {
    case "pending":
    case "running":
      return { state: drain.state } satisfies CliServerDrain;
    case "finished":
      return { state: "finished", report: drain.report } satisfies CliServerDrain;
    default:
      return {
        state: "ended",
        code: drain.endCode,
        message: drain.endCode === "refused" ? drain.refusalMessage : CLI_END_MESSAGES[drain.endCode],
      } satisfies CliServerDrain;
  }
});

/** Each Server's latest Drain in the Organization, keyed by Machine ID. */
export const listLatestServerDrains = Effect.fn("ServerDrain.listLatest")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.selectDistinctOn([serverDrainAttempt.machineId]).from(serverDrainAttempt)
    .where(eq(serverDrainAttempt.organizationId, organization.id))
    .orderBy(serverDrainAttempt.machineId, desc(serverDrainAttempt.requestedAt), desc(serverDrainAttempt.id));
  const latest = yield* Effect.forEach(rows, (row) => Effect.map(latestDrainOf(row), (drain) => [row.machineId, drain] as const));
  return { servers: Object.fromEntries(latest) };
});

/** Whether a Cloud Drain is pending or running on the Server: turning services back on would undo it. */
export const drainActiveOn = Effect.fn("ServerDrain.activeOn")(function* (organizationId: string, machineId: MachineId) {
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
 * A Drain waits its turn after its gate, and an earlier Drain can leave this one retiring a last copy. So it plans
 * again before it runs, and runs only if what it would destroy now is what its gate admitted. Also whether the plan
 * moves anything, which decides the Servers it may land on.
 */
const stillCovered = Effect.fn("ServerDrain.stillCovered")(function* (request: DrainRequest, planDigest: string | null) {
  const plan = yield* planDrain(request.organizationId, request.machineId);
  const moves = plan === null || plan.moves.length > 0;
  return { covered: plan === null || plan.effects.length === 0 || planDigest === operationDigest(plan), moves };
});

/**
 * Ask the Engine once. The row is claimed (`running`) right before the call and ended with the Engine's answer right
 * after, in one step: a retry that finds the row running knows the Engine may have been asked, closes it lost
 * and never asks again; one that finds it ended returns what's there. An error from the Engine means nothing moved
 * (its preflight refused), so the row ends refused. An answer replaces an unknown end (a cancellation that couldn't
 * know what moved) but never another answer, and a Drain with no answer in a day ends lost here.
 */
export const executeDrainOnce = Effect.fn("ServerDrain.execute")(function* (request: DrainRequest, runId: string) {
  const { drizzle } = yield* Database;
  const owned = and(eq(serverDrainAttempt.id, request.attemptId), eq(serverDrainAttempt.inngestRunId, runId));
  const current = yield* stateOf(request.attemptId);
  if (current.state === "running") {
    yield* drizzle.update(serverDrainAttempt)
      .set(ended("lost"))
      .where(and(owned, eq(serverDrainAttempt.state, "running")));
    return yield* stateOf(request.attemptId);
  }
  if (current.state !== "pending") return current;
  const [row] = yield* drizzle.select({ targets: serverDrainAttempt.targets, planDigest: serverDrainAttempt.planDigest })
    .from(serverDrainAttempt).where(eq(serverDrainAttempt.id, request.attemptId));
  const { covered, moves } = yield* stillCovered(request, row?.planDigest ?? null);
  if (!covered) {
    yield* drizzle.update(serverDrainAttempt)
      .set(refused("The drain now destroys something nobody approved. Drain the server again to review it."))
      .where(and(owned, eq(serverDrainAttempt.state, "pending")));
    return yield* stateOf(request.attemptId);
  }
  // The Drain's Container steps run on the drained Server, which retires its Globals, and, when the Drain moves
  // anything, on every other Server taking services, since a move may land on any of them.
  const lacking = yield* serversLackingEnsure(
    request.organizationId,
    (machine) => machine.id === request.machineId || moves && machine.accepts_services,
  );
  if (lacking.length > 0) {
    yield* drizzle.update(serverDrainAttempt)
      .set(refused(`Drains need a newer Ployz on ${lacking.join(", ")}.`))
      .where(and(owned, eq(serverDrainAttempt.state, "pending")));
    return yield* stateOf(request.attemptId);
  }
  const targets = row?.targets ?? null;
  const scope: DrainScope = targets === null
    ? { scope: "owned", namespaces: yield* ownedNamespaces(request.organizationId) }
    : { scope: "services", services: targets };
  const session = yield* openSession(request.organizationId);
  const [claimed] = yield* drizzle.update(serverDrainAttempt).set({ state: "running", startedAt: new Date() })
    .where(and(owned, eq(serverDrainAttempt.state, "pending"))).returning({ id: serverDrainAttempt.id });
  if (claimed === undefined) return yield* stateOf(request.attemptId);
  const end = yield* session.drainMachine(request.machineId, scope).pipe(
    Effect.map((report) => ({ state: "finished", report, endedAt: new Date(), endCode: null, refusalMessage: null }) as const),
    Effect.catch((error) => Effect.succeed(refused(refusalMessage(error)))),
    Effect.timeoutOrElse({ duration: DRAIN_RUNNING_LIMIT_MS, orElse: () => Effect.succeed(ended("lost")) }),
  );
  // The answer is in hand: the write is retried here rather than by asking the Engine again.
  const [written] = yield* drizzle.update(serverDrainAttempt).set(end)
    .where(and(owned, inArray(serverDrainAttempt.state, ["running", "unknown"]))).returning({ state: serverDrainAttempt.state })
    .pipe(Effect.retry({ times: 4, schedule: Schedule.exponential("500 millis") }));
  return written === undefined ? yield* stateOf(request.attemptId) : { attemptId: request.attemptId, state: written.state };
}, Effect.scoped);

/** How a run that ended for good closes its row: by whether the row ever asked the Engine. */
const RUN_ENDS = {
  failure: { pending: "not_started", running: "lost" },
  cancellation: { pending: "cancelled", running: "interrupted" },
} as const satisfies Record<string, Record<"pending" | "running", DrainEndCodeWithoutMessage>>;

/**
 * A run that failed for good or was cancelled must not leave its row active. A pending row never asked the Engine
 * (`not_started`, `cancelled`); a running one may have, so it ends `lost` or `interrupted`, both unknown: Cloud can't undo what the Engine did, and
 * Running here shows it. A run cancelled before it bound its row names the row by `attemptId` from its event. How many
 * rows this call ended.
 */
export const closeDrainRun = Effect.fn("ServerDrain.closeRun")(function* (
  runId: string,
  reason: keyof typeof RUN_ENDS,
  attemptId?: string,
) {
  const { drizzle } = yield* Database;
  const { pending, running } = RUN_ENDS[reason];
  const isPending = sql`${serverDrainAttempt.state} = 'pending'`;
  const unbound = attemptId === undefined ? undefined : and(
    eq(serverDrainAttempt.id, attemptId),
    eq(serverDrainAttempt.state, "pending"),
    or(isNull(serverDrainAttempt.inngestRunId), eq(serverDrainAttempt.inngestRunId, runId)),
  );
  const rows = yield* drizzle.update(serverDrainAttempt).set({
    state: sql<DrainState>`case when ${isPending} then ${DRAIN_END_CODES[pending]} else ${DRAIN_END_CODES[running]} end`,
    endCode: sql<DrainEndCode>`case when ${isPending} then ${pending} else ${running} end`,
    endedAt: new Date(),
  })
    .where(or(and(eq(serverDrainAttempt.inngestRunId, runId), inArray(serverDrainAttempt.state, ACTIVE_STATES)), unbound))
    .returning({ id: serverDrainAttempt.id });
  return rows.length;
});

/**
 * The hourly sweep: a request no run picked up in time never starts; a Drain running for a day lost its run. Drains in
 * an Organization run one at a time, so a request waits its turn while one runs there or an earlier request is ahead
 * of it, and its time runs from when the Drain before it ended.
 */
export const closeStaleDrains = Effect.fn("ServerDrain.closeStale")(function* () {
  const { drizzle } = yield* Database;
  const now = Date.now();
  const unknown = yield* drizzle.update(serverDrainAttempt)
    .set(ended("lost"))
    .where(and(eq(serverDrainAttempt.state, "running"), lt(serverDrainAttempt.startedAt, new Date(now - DRAIN_RUNNING_LIMIT_MS))))
    .returning({ id: serverDrainAttempt.id });
  const failed = yield* drizzle.update(serverDrainAttempt)
    .set(ended("not_started"))
    .where(and(
      eq(serverDrainAttempt.state, "pending"),
      // Spelled out: an update renders its columns unqualified, which inside a subquery would name the alias's.
      sql`not exists (
        select 1 from server_drain_attempt as ahead
        where ahead.organization_id = server_drain_attempt.organization_id
          and (ahead.state = 'running' or (ahead.state = 'pending' and ahead.requested_at < server_drain_attempt.requested_at))
      )`,
      sql`greatest(server_drain_attempt.requested_at, (
        select max(before.ended_at) from server_drain_attempt as before
        where before.organization_id = server_drain_attempt.organization_id
      )) < ${new Date(now - DRAIN_PENDING_LIMIT_MS)}`,
    ))
    .returning({ id: serverDrainAttempt.id });
  return { failed: failed.length, unknown: unknown.length };
});
