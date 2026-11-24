import "@tanstack/react-start/server-only";
import type { DrainReport, DrainScope, MachineId, MachineName, QualifiedService, RuntimeWatchView } from "@ployz/sdk";
import { and, desc, eq, sql } from "drizzle-orm";
import { Clock, Data, Effect, Option } from "effect";
import { SERVERS_UNANSWERED_MESSAGE } from "#/lib/refusal";
import { listNames } from "#/lib/plural";
import { gateOperation, operationDigest } from "#/modules/operations/gate.server";
import type { Actor, Caller } from "#/modules/identity/actor";
import { type ServerDrainRequestedEventData, serverDrainRequested } from "#/modules/machines/machines.events";
import { ownedNamespaces } from "#/modules/config-store/store-namespaces.server";
import { attemptRows, latestPerMachine, queueWaitedPast, ready, settled, within } from "#/modules/inngest/attempt-row.server";
import { enqueueEvent } from "#/modules/inngest/outbox.server";
import { DRAIN_PENDING_LIMIT_MS, DRAIN_RUNNING_LIMIT_MS, type LatestDrain, type RequestedServerDrain, type RequestServerDrainInput } from "#/modules/machines/server-drain";
import {
  DRAIN_END_CODES,
  DRAIN_END_WORDS,
  type DrainEndCode,
  type DrainEndCodeWithoutMessage,
  type DrainState,
} from "#/modules/machines/server-drain-states";
import { drainPlan, planDrain } from "#/modules/machines/server-operations.server";
import { untried } from "#/modules/machines/server-drain-volumes";
import { readVolumeLines, targetedMessage, volumeRunTargeting } from "#/modules/machines/server-drain-volumes.server";
import { splitQualifiedService } from "#/modules/machines/server-services";
import { serverDrainAttempt } from "#/modules/machines/tables";
import { lackingEnsure } from "#/modules/runtime/daemon-compat.server";
import { requireAccess } from "#/modules/organization/access.server";
import { deployDialOf } from "#/modules/config-store/store-sdk.server";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { clusterRead, OrganizationRuntime, RUNTIME_FRAME_TIMEOUT_MS } from "#/modules/runtime/organization-runtime.server";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";

class ServerDrainUnreachable extends Data.TaggedError("ServerDrainUnreachable")<{
  readonly operation: string;
  readonly cause: unknown;
}> {}

type DrainAttemptRow = typeof serverDrainAttempt.$inferSelect;
type DrainRequest = ServerDrainRequestedEventData;
/** What a run step answers with: the row's state once the step is done with it. */
type DrainRunReply = { readonly attemptId: string; readonly state: DrainState };

const rows = attemptRows(serverDrainAttempt, "The drain request was not found.");

/** A row as the page reads it. The check constraint keeps each state's evidence; a row without it is a defect. */
export function latestDrainOf(row: DrainAttemptRow): Effect.Effect<LatestDrain> {
  const attemptId = row.id;
  const missing = (what: string) => Effect.die(new Error(`Drain ${attemptId} is ${row.state} without its ${what}.`));
  switch (row.state) {
    case "pending":
      return Effect.succeed({ attemptId, state: "pending", requestedAt: row.requestedAt.toISOString(), serversSilent: row.serversSilent });
    case "running":
      return row.startedAt === null
        ? missing("start time")
        : Effect.succeed({ attemptId, state: "running", startedAt: row.startedAt.toISOString(), report: row.report, lines: {} });
    case "finished":
      return row.endedAt === null || row.report === null
        ? missing("report")
        : Effect.succeed({ attemptId, state: "finished", endedAt: row.endedAt.toISOString(), report: row.report, lines: {} });
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

/** A row whose Engine call already answered ends finished, keeping that answer, whatever ends it. */
const keepsReport = sql`${serverDrainAttempt.report} is not null`;

/** The columns that end a row under `code`; its state follows from the code. */
const ended = (code: DrainEndCodeWithoutMessage) =>
  ({ state: DRAIN_END_CODES[code], endCode: code, refusalMessage: null, endedAt: sql`now()` }) as const;
const refused = (message: string) =>
  ({ state: DRAIN_END_CODES.refused, endCode: "refused", refusalMessage: message, endedAt: sql`now()` }) as const;

/** Asks for the row's run in the caller's transaction; the outbox sends it once that commits. */
const enqueueDrain = (row: DrainAttemptRow) =>
  enqueueEvent(serverDrainRequested.create({ attemptId: row.id, organizationId: row.organizationId, machineId: row.machineId }));

/**
 * A Volume moving onto the Server would start its Service there, which a Drain would then have to move again, so a
 * Drain is refused while one targets it. Checked at the request and again right before the Engine is asked.
 */
const refuseWhileTargeted = Effect.fn("ServerDrain.refuseWhileTargeted")(function* (organizationId: string, machineId: MachineId) {
  const targeted = yield* volumeRunTargeting(organizationId, machineId);
  if (targeted !== null) return yield* new Conflict({ userFacing: true, message: targetedMessage(targeted) });
});

/**
 * Drain on a Server page: an owner or admin may, and confirming it is their approval, so a plan that now stops a Service
 * the dialog didn't show as stopping is refused. The admitted Drain's row takes the id
 * the tab minted and is written with its run's event, so the same request again returns it. A click while another
 * Drain is active on the Server returns that one.
 */
export const requestServerDrain = Effect.fn("ServerDrain.request")(function* (caller: Caller, input: RequestServerDrainInput) {
  yield* requireAccess(caller, caller.organization.slug, "servers:operate");
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
      rows.active,
    )).orderBy(desc(serverDrainAttempt.requestedAt)).limit(1);
    return active === undefined ? null : { drain: yield* latestDrainOf(active) } satisfies RequestedServerDrain;
  });
  const existing = yield* sameOrActive();
  if (existing !== null) return existing;
  yield* refuseWhileTargeted(organizationId, input.machineId);
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
      serverName: plan.name || null,
      requestedByUserId: caller.userId,
      targets: [...plan.targets],
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
 * Answers the Drain already active on the Server instead of a second.
 */
export const requestCliServerDrain = Effect.fn("ServerDrain.requestCli")(function* (
  caller: { readonly organizationId: string; readonly userId: string },
  input: {
    readonly attemptId: string;
    readonly machineId: MachineId;
    readonly targets: readonly QualifiedService[];
    readonly planDigest: string;
    /** A person said yes to `planDigest`: the Drain runs only while its plan is exactly that one. */
    readonly approved: boolean;
    /** The name the caller showed for the Server, which its run keeps; null when it showed none. */
    readonly serverName: MachineName | null;
  },
) {
  const { drizzle } = yield* Database;
  const [same] = yield* drizzle.select({ id: serverDrainAttempt.id }).from(serverDrainAttempt).where(eq(serverDrainAttempt.id, input.attemptId));
  if (same === undefined) yield* refuseWhileTargeted(caller.organizationId, input.machineId);
  const [inserted] = yield* drizzle.insert(serverDrainAttempt).values({
    id: input.attemptId,
    organizationId: caller.organizationId,
    machineId: input.machineId,
    serverName: input.serverName || null,
    requestedByUserId: caller.userId,
    targets: [...input.targets],
    planDigest: input.planDigest,
    approved: input.approved,
  }).onConflictDoNothing().returning();
  if (inserted !== undefined) {
    yield* enqueueDrain(inserted);
    return inserted.id;
  }
  const [active] = yield* drizzle.select().from(serverDrainAttempt).where(and(
    eq(serverDrainAttempt.organizationId, caller.organizationId),
    eq(serverDrainAttempt.machineId, input.machineId),
    rows.active,
  )).limit(1);
  if (active === undefined) return yield* new Conflict({ userFacing: true, message: "A drain on this server just ended. Try again." });
  return active.id;
});

/** Why a Drain ended without a report: the Engine's refusal, or Cloud's words for an end it named. Null otherwise. */
export const drainEndMessage = (row: Pick<DrainAttemptRow, "endCode" | "refusalMessage">) =>
  row.endCode === null || row.endCode === "refused" ? row.refusalMessage : DRAIN_END_WORDS[row.endCode].cli;

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
      return { state: "finished", report: (yield* readVolumeLines(organizationId, drain.report, true)).report } satisfies CliServerDrain;
    default:
      return {
        state: "ended",
        code: drain.endCode,
        message: drain.endCode === "refused" ? drain.refusalMessage : DRAIN_END_WORDS[drain.endCode].cli,
      } satisfies CliServerDrain;
  }
});

/** Each Server's latest Drain in the Organization, keyed by Machine ID. */
export const listLatestServerDrains = Effect.fn("ServerDrain.listLatest")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "servers:read");
  const latestRows = yield* latestPerMachine(serverDrainAttempt, organization.id);
  const latest = yield* Effect.forEach(latestRows, (row) => Effect.gen(function* () {
    const drain = yield* latestDrainOf(row);
    if ((drain.state !== "running" && drain.state !== "finished") || drain.report === null) return [row.machineId, drain] as const;
    const { report, lines } = yield* readVolumeLines(organization.id, drain.report, drain.state === "finished");
    return [row.machineId, { ...drain, report, lines }] as const;
  }));
  return { servers: Object.fromEntries(latest) };
});

/**
 * What a Drain of the Server would now do with each Service its data holds there, for the confirm dialog: a forecast
 * from one frame, which the Drain checks again before it moves anything.
 */
export const previewServerDrain = Effect.fn("ServerDrain.preview")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly machineId: MachineId },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "servers:read");
  const plan = yield* planDrain(organization.id, input.machineId);
  return { data: plan?.data ?? {} };
});

/** Whether a Cloud Drain is pending or running on the Server: turning services back on would undo it. */
export const drainActiveOn = Effect.fn("ServerDrain.activeOn")(function* (organizationId: string, machineId: MachineId) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ id: serverDrainAttempt.id }).from(serverDrainAttempt).where(and(
    eq(serverDrainAttempt.organizationId, organizationId),
    eq(serverDrainAttempt.machineId, machineId),
    rows.active,
  )).limit(1);
  return row !== undefined;
});

/**
 * Bind the run to its pending row, so only it may claim and end the row. The event must name the row's Organization
 * and Server. A row that is no longer pending (bound by another run, or already ended) is settled: the run does nothing.
 */
export const bindDrainRun = (request: DrainRequest, runId: string) =>
  rows.bind(request.attemptId, runId, [
    eq(serverDrainAttempt.organizationId, request.organizationId),
    eq(serverDrainAttempt.machineId, request.machineId),
  ]);

/** The Cluster as the Drain finds it when its turn comes: the frame it plans from. */
const reachCluster = Effect.fn("ServerDrain.reachCluster")(function* (organizationId: string) {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  // No Cluster paired is nothing to wait for.
  if (session.status === "no_connection") return yield* new Conflict({ userFacing: true, message: SERVERS_UNANSWERED_MESSAGE });
  if (session.status !== "connected") {
    return yield* new ServerDrainUnreachable({ operation: "open organization runtime", cause: session.status });
  }
  const frame = yield* session.connected.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS).pipe(
    clusterRead,
    Effect.mapError((cause) => new ServerDrainUnreachable({ operation: "read the cluster", cause })),
  );
  return { frame };
});

/** What the Engine said when it refused, within the row's limit. */
function refusalMessage(cause: Error) {
  const message = cause.message.trim();
  return within(message.length > 0 ? message : "The drain was refused.");
}

/**
 * A Drain waits its turn after its gate, and an earlier Drain can leave this one retiring a last copy. So it plans
 * again before it runs, and runs only if what it would destroy now is what its gate admitted. One a person said yes to
 * runs only its exact plan, as its gate did. Also whether the plan moves anything, which decides the Servers it may land on.
 */
const stillCovered = Effect.fn("ServerDrain.stillCovered")(function* (
  request: DrainRequest,
  frame: RuntimeWatchView,
  admitted: { readonly planDigest: string | null; readonly approved: boolean },
) {
  const plan = drainPlan(frame, new Set(yield* ownedNamespaces(request.organizationId)), request.machineId);
  const moves = plan === null || plan.moves.length > 0;
  // A yes covers only the plan it was given: one whose Server left the frame, so nothing plans, runs nothing.
  if (admitted.approved) return { covered: plan !== null && admitted.planDigest === operationDigest(plan), moves };
  const harmless = plan === null || plan.effects.length === 0;
  return { covered: harmless || admitted.planDigest === operationDigest(plan), moves };
});

/**
 * Ask the Engine once (see `attemptRows`' `executeOnce`). The Engine's own error means nothing moved (its preflight
 * refused), so the row ends refused; any other rejection may follow a move, so the row ends lost. An answer replaces an
 * unknown end (a cancellation that couldn't know what moved) but never another answer, and a Drain with no answer in a
 * day ends lost here.
 *
 * A Cluster that doesn't answer before the claim leaves the row pending and marks it silent, for the run to try again
 * after a wait, until `DRAIN_PENDING_LIMIT_MS` after the request. A try that reaches the Cluster unmarks it first, so
 * a step after that which fails and retries doesn't read as silent. Past the limit, a silent Cluster ends the row
 * `not_started` here, so no retry can drain it later, and a try after a wait ends it without asking. A first try past
 * it still drains if the Cluster answers, as a Drain whose turn came late always has.
 */
export const executeDrainOnce = Effect.fn("ServerDrain.execute")(function* (
  request: DrainRequest,
  runId: string,
  { waited }: { readonly waited: boolean },
) {
  const { drizzle } = yield* Database;
  const id = request.attemptId;
  const owned = rows.owned(id, runId);
  // A retry after the Engine answered resumes the Volume moves from the answer it kept, never asking again.
  const [answered] = yield* drizzle.select({ id: serverDrainAttempt.id }).from(serverDrainAttempt)
    .where(and(owned, eq(serverDrainAttempt.state, "running"), sql`${serverDrainAttempt.report} is not null`));
  if (answered !== undefined) return { attemptId: id, state: "running" } satisfies DrainRunReply;
  const state = yield* rows.executeOnce({
    id,
    runId,
    lost: ended("lost"),
    endsFrom: ["running", "unknown"],
    prepare: Effect.gen(function* () {
      const [row] = yield* drizzle.select({
        targets: serverDrainAttempt.targets,
        planDigest: serverDrainAttempt.planDigest,
        approved: serverDrainAttempt.approved,
        approvalId: serverDrainAttempt.approvalId,
        requestedAt: serverDrainAttempt.requestedAt,
      }).from(serverDrainAttempt).where(eq(serverDrainAttempt.id, id));
      const deadline = row === undefined ? 0 : row.requestedAt.getTime() + DRAIN_PENDING_LIMIT_MS;
      const expired = Effect.map(Clock.currentTimeMillis, (now) => now > deadline);
      /** End the pending row with `values`; the row's state, whoever ended it. */
      const endPending = Effect.fn(function* (values: Parameters<typeof rows.end>[2]) {
        return settled((yield* rows.end(id, runId, values, ["pending"])) ?? (yield* rows.stateOf(id)));
      });
      if (waited && (yield* expired)) return yield* endPending(ended("not_started"));
      const reached = yield* reachCluster(request.organizationId).pipe(
        Effect.map(Option.some),
        Effect.catchIf((error) => error instanceof ServerDrainUnreachable, () => Effect.succeedNone),
      );
      if (Option.isNone(reached)) {
        // Reading a silent Cluster takes time, so the deadline may pass during the read.
        if (yield* expired) return yield* endPending(ended("not_started"));
        yield* drizzle.update(serverDrainAttempt).set({ serversSilent: true }).where(and(owned, eq(serverDrainAttempt.state, "pending")));
        return settled("pending" as const);
      }
      const { frame } = reached.value;
      yield* drizzle.update(serverDrainAttempt).set({ serversSilent: false })
        .where(and(owned, eq(serverDrainAttempt.state, "pending"), eq(serverDrainAttempt.serversSilent, true)));
      // A row an older worker wrote, or one from before `approved`, names the approval it consumed instead: a yes too.
      const approved = row !== undefined && (row.approved || row.approvalId !== null);
      const { covered, moves } = yield* stillCovered(request, frame, { planDigest: row?.planDigest ?? null, approved });
      if (!covered) {
        const changed = approved ? "The drain changed since you said yes." : "The drain now destroys something nobody approved.";
        return yield* endPending(refused(`${changed} Drain the server again to review it.`));
      }
      // The Drain's Container steps run on the drained Server, which retires its Globals, and, when the Drain moves
      // anything, on every other Server taking services, since a move may land on any of them.
      const lacking = lackingEnsure(frame, (machine) => machine.id === request.machineId || moves && machine.accepts_services);
      if (lacking.length > 0) return yield* endPending(refused(`Drains need a newer Ployz on ${lacking.join(", ")}.`));
      const targeted = yield* volumeRunTargeting(request.organizationId, request.machineId);
      if (targeted !== null) return yield* endPending(refused(targetedMessage(targeted)));
      const targets = row?.targets ?? null;
      const scope: DrainScope = targets === null
        ? { scope: "owned", namespaces: yield* ownedNamespaces(request.organizationId) }
        : { scope: "services", services: targets };
      // The Store sends each copy it moves with the signed versions it keeps for that copy's spec. Both are read while
      // the row is pending, so a failure here is retried rather than ending a drain the Engine was never asked for.
      const store = yield* cloudStore;
      const { connections, expect } = yield* deployDialOf(request.organizationId);
      return ready({ scope, store, connections, expect });
    }),
    call: ({ scope, store, connections, expect }) =>
      storeTry(() => store.drainMachine(request.organizationId, request.machineId, scope, connections, expect)).pipe(
        // The answer is kept on the running row while the drain moves its Volumes. A row a cancellation already ended
        // unknown ends finished with it instead, and moves nothing.
        Effect.map((report) => ({
          report: untried(report),
          state: sql<DrainState>`case when ${serverDrainAttempt.state} = 'unknown' then 'finished' else ${serverDrainAttempt.state} end`,
          endCode: null,
          refusalMessage: null,
        })),
        // The Engine's own error comes from its input checks, the dial or its preflight, before anything moved. Anything
        // else may come after copies moved, so what the drain did is unknown.
        Effect.catch((refusal) => Effect.succeed(refused(refusalMessage(refusal)))),
        Effect.catchDefect(() => Effect.succeed(ended("lost"))),
        Effect.timeoutOrElse({ duration: DRAIN_RUNNING_LIMIT_MS, orElse: () => Effect.succeed(ended("lost")) }),
      ),
  });
  return { attemptId: id, state } satisfies DrainRunReply;
}, Effect.scoped);

/** How a run that ended for good closes its row: by whether the row ever asked the Engine. */
const RUN_ENDS = {
  failure: { pending: "not_started", running: "lost" },
  cancellation: { pending: "cancelled", running: "interrupted" },
} as const satisfies Record<string, Record<"pending" | "running", DrainEndCodeWithoutMessage>>;

/**
 * A run that failed for good or was cancelled must not leave its row active. A pending row never asked the Engine
 * (`not_started`, `cancelled`); a running one may have, so it ends `lost` or `interrupted`, both unknown: Cloud can't
 * undo what the Engine did, and Running here shows it. A running row that kept the Engine's answer ends finished with
 * it: a Volume move it admitted runs on, and the read gives each Volume's line from that move. A run cancelled before
 * it bound its row names the row by `attemptId` from its event. How many rows this call ended.
 */
export const closeDrainRun = (runId: string, reason: keyof typeof RUN_ENDS, attemptId?: string) => {
  const { pending, running } = RUN_ENDS[reason];
  return rows.closeRun(runId, attemptId, (whenPending) => ({
    state: sql<DrainState>`case when ${keepsReport} then 'finished' else ${whenPending(DRAIN_END_CODES[pending], DRAIN_END_CODES[running])} end`,
    endCode: sql<DrainEndCode | null>`case when ${keepsReport} then null else ${whenPending(pending, running)} end`,
  }));
};

/**
 * The hourly sweep: a request no run picked up in time never starts; a Drain running for a day lost its run, and ends
 * finished when it kept the Engine's answer. A request waits its turn while a Drain runs in its Organization or an
 * earlier request is ahead of it, and its time runs from when the Drain before it ended. A Drain gives its turn back
 * while it waits on a Volume move, so another may run then, and that one plans again before it asks the Engine.
 */
export const closeStaleDrains = Effect.fn("ServerDrain.closeStale")(function* () {
  const { running, pending } = yield* rows.closeStale({
    running: {
      limitMs: DRAIN_RUNNING_LIMIT_MS,
      end: {
        state: sql<DrainState>`case when ${keepsReport} then 'finished' else 'unknown' end`,
        endCode: sql<DrainEndCode | null>`case when ${keepsReport} then null else 'lost' end`,
        refusalMessage: null,
        endedAt: sql`now()`,
      },
    },
    pending: {
      end: ended("not_started"),
      when: (now) => queueWaitedPast(serverDrainAttempt, new Date(now - DRAIN_PENDING_LIMIT_MS), { earlierOnly: false }),
    },
  });
  return { failed: pending, unknown: running };
});
