import "@tanstack/react-start/server-only";
import { and, eq, inArray, isNull, lt, or, sql } from "drizzle-orm";
import { Data, Effect, Schedule } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { readStore } from "#/modules/config-store/config-store.server";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createNamespaceCleanupRequestedEvent, type NamespaceCleanupRequestedEventData } from "#/modules/inngest/events";
import {
  CLEANUP_END_CODES,
  CLEANUP_PENDING_LIMIT_MS,
  CLEANUP_RUNNING_LIMIT_MS,
  type CleanupEndCode,
  type CleanupEndCodeWithoutMessage,
  type CleanupState,
} from "#/modules/machines/namespace-cleanup";
import { namespaceCleanup } from "#/modules/machines/tables";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";
import { SYSTEM_NAMESPACE } from "./server-services";

/** The Namespaces the Organization's Environments own, as the Store names them. A failed read fails: nothing reads as unowned. */
export const ownedNamespaces = (organizationId: string) =>
  readStore(organizationId, { query: "namespaces" }).pipe(Effect.map(({ namespaces }) => namespaces.map(({ namespace }) => namespace)));

/** Which of `namespaces`, seen on the Organization's Servers, no Environment owns: the Servers page offers to remove them. */
export const listStrayNamespaces = Effect.fn("NamespaceCleanup.strays")(function* (
  actor: Actor, input: { organizationSlug: string; namespaces: readonly string[] },
) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const owned = new Set(yield* ownedNamespaces(organization.id));
  return input.namespaces.filter((namespace) => namespace !== SYSTEM_NAMESPACE && !owned.has(namespace));
});

/**
 * The Organization's Servers, for removing `namespace`: only one no Environment owns, and never Ployz's own. An owned
 * one goes with its Environment (Settings › Delete), which the Store records.
 */
const unownedSession = Effect.fn("NamespaceCleanup.session")(function* (actor: Actor, organizationSlug: string, namespace: string) {
  const organization = yield* requireInfrastructureOrganization(actor, organizationSlug);
  if (namespace === SYSTEM_NAMESPACE || (yield* ownedNamespaces(organization.id)).includes(namespace)) {
    return yield* new Conflict({ userFacing: true, message: `${namespace} belongs to an Environment: delete that Environment in its Settings instead.` });
  }
  const session = yield* (yield* OrganizationRuntime).open(organization.id);
  if (session.status !== "connected") return yield* new Conflict({ userFacing: true, message: "Your servers aren't answering. Try again once they are." });
  return session.connected;
});

/** What removing `namespace` from the Servers deletes: its Volumes' data, on each Server that holds it. */
export const loadNamespaceDataLoss = Effect.fn("NamespaceCleanup.dataLoss")(function* (
  actor: Actor, input: { organizationSlug: string; namespace: string },
) {
  const session = yield* unownedSession(actor, input.organizationSlug, input.namespace);
  const observed = yield* session.dataLossIfNamespaceDestroyed(input.namespace).pipe(
    Effect.mapError(() => new Conflict({ userFacing: true, message: "Couldn't check what this deletes on your servers. Try again." })));
  return observed.data_loss;
});

/**
 * Remove `namespace`, which no Environment owns, from every Server with its Volumes, deleting exactly `confirmDataLoss`.
 * Resolves to what the Servers hold beyond it when they hold more by now, to be confirmed again.
 */
export const removeNamespace = Effect.fn("NamespaceCleanup.remove")(function* (
  actor: Actor, input: { organizationSlug: string; namespace: string; confirmDataLoss: readonly DataLossIdentity[] },
) {
  const session = yield* unownedSession(actor, input.organizationSlug, input.namespace);
  const outcome = yield* session.destroyNamespace(input.namespace, { confirmed: [...input.confirmDataLoss] }).pipe(
    Effect.map((done) => done.type === "success" ? { removed: true as const } : { failed: "Some servers didn't finish removing it. Try again." }),
    Effect.catchTag("MissingDataLossIdentities", (missing) => Effect.succeed({ missing: missing.identities })),
    Effect.mapError(() => new Conflict({ userFacing: true, message: "Your servers didn't answer. Try again." })),
  );
  return outcome;
}, Effect.scoped);

export class NamespaceCleanupUnreachable extends Data.TaggedError("NamespaceCleanupUnreachable")<{
  readonly cause: unknown;
}> {}

type CleanupRow = typeof namespaceCleanup.$inferSelect;
type CleanupRequest = NamespaceCleanupRequestedEventData;
/** What a run step answers with: the row's state once the step is done with it. */
export type CleanupRunReply = { readonly cleanupId: string; readonly state: CleanupState };

const ACTIVE_STATES: readonly CleanupState[] = ["pending", "running"];
const MESSAGE_LIMIT = 1024;

const ended = (code: CleanupEndCodeWithoutMessage) =>
  ({ state: CLEANUP_END_CODES[code], endCode: code, endMessage: null, endedAt: new Date() }) as const;
const endedSaying = (code: "refused" | "incomplete", message: string) => ({
  state: CLEANUP_END_CODES[code],
  endCode: code,
  endMessage: message.length > MESSAGE_LIMIT ? `${message.slice(0, MESSAGE_LIMIT - 1)}…` : message,
  endedAt: new Date(),
}) as const;

const dispatch = Effect.fn("NamespaceCleanup.dispatch")(function* (row: CleanupRow) {
  const { drizzle } = yield* Database;
  yield* sendInngestEvent(createNamespaceCleanupRequestedEvent({ cleanupId: row.id, organizationId: row.organizationId })).pipe(
    Effect.tapError(() =>
      drizzle.update(namespaceCleanup)
        .set(ended("not_started"))
        .where(and(eq(namespaceCleanup.id, row.id), eq(namespaceCleanup.state, "pending"), isNull(namespaceCleanup.inngestRunId)))),
  );
});

/**
 * A clean the signed-in CLI asked for, deleting exactly the Volumes it previewed. A retry carrying the approval this
 * clean consumed answers its row; a request while another clean of the Namespace is active answers that one.
 */
export const requestNamespaceCleanup = Effect.fn("NamespaceCleanup.request")(function* (
  caller: { readonly organizationId: string; readonly userId: string },
  input: { readonly namespace: string; readonly confirmDataLoss: readonly DataLossIdentity[]; readonly approvalId: string | null },
) {
  const { drizzle } = yield* Database;
  const [inserted] = yield* drizzle.insert(namespaceCleanup).values({
    id: crypto.randomUUID(),
    organizationId: caller.organizationId,
    namespace: input.namespace,
    requestedByUserId: caller.userId,
    approvalId: input.approvalId,
    confirmDataLoss: [...input.confirmDataLoss],
  }).onConflictDoNothing().returning();
  if (inserted !== undefined) {
    yield* dispatch(inserted);
    return inserted.id;
  }
  const consumed = input.approvalId === null ? [] : yield* drizzle.select().from(namespaceCleanup).where(and(
    eq(namespaceCleanup.organizationId, caller.organizationId),
    eq(namespaceCleanup.approvalId, input.approvalId),
  ));
  const [active] = consumed.length > 0 ? consumed : yield* drizzle.select().from(namespaceCleanup).where(and(
    eq(namespaceCleanup.organizationId, caller.organizationId),
    eq(namespaceCleanup.namespace, input.namespace),
    inArray(namespaceCleanup.state, ACTIVE_STATES),
  )).limit(1);
  if (active === undefined) return yield* new Conflict({ userFacing: true, message: "A clean of this namespace just ended. Try again." });
  if (active.state === "pending" && active.inngestRunId === null) yield* dispatch(active);
  return active.id;
});

/** Bind the run to its pending row, so only it may claim and end the row. A row no longer pending is settled. */
export const bindCleanupRun = Effect.fn("NamespaceCleanup.bind")(function* (request: CleanupRequest, runId: string) {
  const { drizzle } = yield* Database;
  const [bound] = yield* drizzle.update(namespaceCleanup).set({ inngestRunId: runId }).where(and(
    eq(namespaceCleanup.id, request.cleanupId),
    eq(namespaceCleanup.organizationId, request.organizationId),
    eq(namespaceCleanup.state, "pending"),
    or(isNull(namespaceCleanup.inngestRunId), eq(namespaceCleanup.inngestRunId, runId)),
  )).returning({ id: namespaceCleanup.id });
  if (bound !== undefined) return { kind: "bound" } as const;
  const [row] = yield* drizzle.select({ state: namespaceCleanup.state, organizationId: namespaceCleanup.organizationId })
    .from(namespaceCleanup).where(eq(namespaceCleanup.id, request.cleanupId));
  if (row === undefined || row.organizationId !== request.organizationId) {
    return yield* new NotFound({ message: "The namespace clean was not found." });
  }
  return { kind: "settled", state: row.state } as const;
});

const stateOf = Effect.fn("NamespaceCleanup.stateOf")(function* (cleanupId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ state: namespaceCleanup.state }).from(namespaceCleanup).where(eq(namespaceCleanup.id, cleanupId));
  if (row === undefined) return yield* new NotFound({ message: "The namespace clean was not found." });
  return { cleanupId, state: row.state } satisfies CleanupRunReply;
});

export const executeCleanupOnce = Effect.fn("NamespaceCleanup.execute")(function* (request: CleanupRequest, runId: string) {
  const { drizzle } = yield* Database;
  const owned = and(eq(namespaceCleanup.id, request.cleanupId), eq(namespaceCleanup.inngestRunId, runId));
  const current = yield* stateOf(request.cleanupId);
  if (current.state === "running") {
    yield* drizzle.update(namespaceCleanup).set(ended("lost")).where(and(owned, eq(namespaceCleanup.state, "running")));
    return yield* stateOf(request.cleanupId);
  }
  if (current.state !== "pending") return current;
  const [row] = yield* drizzle.select({ namespace: namespaceCleanup.namespace, confirmDataLoss: namespaceCleanup.confirmDataLoss })
    .from(namespaceCleanup).where(eq(namespaceCleanup.id, request.cleanupId));
  if (row === undefined) return yield* stateOf(request.cleanupId);
  const session = yield* (yield* OrganizationRuntime).open(request.organizationId);
  if (session.status !== "connected") return yield* new NamespaceCleanupUnreachable({ cause: session.status });
  if (row.namespace === SYSTEM_NAMESPACE || (yield* ownedNamespaces(request.organizationId)).includes(row.namespace)) {
    const [refused] = yield* drizzle.update(namespaceCleanup)
      .set(endedSaying("refused", `${row.namespace} belongs to an Environment now. Nothing was removed.`))
      .where(and(owned, eq(namespaceCleanup.state, "pending"))).returning({ state: namespaceCleanup.state });
    return refused === undefined ? yield* stateOf(request.cleanupId) : { cleanupId: request.cleanupId, state: refused.state };
  }
  const [claimed] = yield* drizzle.update(namespaceCleanup).set({ state: "running", startedAt: new Date() })
    .where(and(owned, eq(namespaceCleanup.state, "pending"))).returning({ id: namespaceCleanup.id });
  if (claimed === undefined) return yield* stateOf(request.cleanupId);
  const volumes = row.confirmDataLoss.map(({ id }) => id.name).sort();
  const end = yield* session.connected.destroyNamespace(row.namespace, { confirmed: row.confirmDataLoss }).pipe(
    Effect.map((outcome) => outcome.type === "success"
      ? ({ state: "finished", volumes, endedAt: new Date(), endCode: null, endMessage: null }) as const
      : endedSaying("incomplete", "Some servers didn't finish removing the namespace.")),
    Effect.catchTag("MissingDataLossIdentities", () =>
      Effect.succeed(endedSaying("refused", "The servers hold Volumes this clean didn't confirm. Nothing was removed."))),
    Effect.catch(() => Effect.succeed(ended("lost"))),
    Effect.timeoutOrElse({ duration: CLEANUP_RUNNING_LIMIT_MS, orElse: () => Effect.succeed(ended("lost")) }),
  );
  const [written] = yield* drizzle.update(namespaceCleanup).set(end)
    .where(and(owned, inArray(namespaceCleanup.state, ["running", "unknown"]))).returning({ state: namespaceCleanup.state })
    .pipe(Effect.retry({ times: 4, schedule: Schedule.exponential("500 millis") }));
  return written === undefined ? yield* stateOf(request.cleanupId) : { cleanupId: request.cleanupId, state: written.state };
}, Effect.scoped);

const RUN_ENDS = {
  failure: { pending: "not_started", running: "lost" },
  cancellation: { pending: "cancelled", running: "interrupted" },
} as const satisfies Record<string, Record<"pending" | "running", CleanupEndCodeWithoutMessage>>;

/**
 * A run that failed for good or was cancelled must not leave its row active. A run cancelled before it bound its row
 * names the row by `cleanupId` from its event. How many rows this call ended.
 */
export const closeCleanupRun = Effect.fn("NamespaceCleanup.closeRun")(function* (
  runId: string,
  reason: keyof typeof RUN_ENDS,
  cleanupId?: string,
) {
  const { drizzle } = yield* Database;
  const { pending, running } = RUN_ENDS[reason];
  const isPending = sql`${namespaceCleanup.state} = 'pending'`;
  const unbound = cleanupId === undefined ? undefined : and(
    eq(namespaceCleanup.id, cleanupId),
    eq(namespaceCleanup.state, "pending"),
    or(isNull(namespaceCleanup.inngestRunId), eq(namespaceCleanup.inngestRunId, runId)),
  );
  const rows = yield* drizzle.update(namespaceCleanup).set({
    state: sql<CleanupState>`case when ${isPending} then ${CLEANUP_END_CODES[pending]} else ${CLEANUP_END_CODES[running]} end`,
    endCode: sql<CleanupEndCode>`case when ${isPending} then ${pending} else ${running} end`,
    endedAt: new Date(),
  })
    .where(or(and(eq(namespaceCleanup.inngestRunId, runId), inArray(namespaceCleanup.state, ACTIVE_STATES)), unbound))
    .returning({ id: namespaceCleanup.id });
  return rows.length;
});

/** The hourly sweep: a request no run picked up in time never starts; a clean running this long lost its run. */
export const closeStaleCleanups = Effect.fn("NamespaceCleanup.closeStale")(function* () {
  const { drizzle } = yield* Database;
  const now = Date.now();
  const unknown = yield* drizzle.update(namespaceCleanup).set(ended("lost"))
    .where(and(eq(namespaceCleanup.state, "running"), lt(namespaceCleanup.startedAt, new Date(now - CLEANUP_RUNNING_LIMIT_MS))))
    .returning({ id: namespaceCleanup.id });
  const failed = yield* drizzle.update(namespaceCleanup).set(ended("not_started"))
    .where(and(eq(namespaceCleanup.state, "pending"), lt(namespaceCleanup.requestedAt, new Date(now - CLEANUP_PENDING_LIMIT_MS))))
    .returning({ id: namespaceCleanup.id });
  return { failed: failed.length, unknown: unknown.length };
});

const CLI_END_MESSAGES = {
  not_started: "The clean never started.",
  cancelled: "The clean was cancelled before it started.",
  interrupted: "The clean was cancelled while it ran; some Volumes may be gone.",
  lost: "Cloud lost track of the clean; some Volumes may be gone.",
} as const satisfies Record<CleanupEndCodeWithoutMessage, string>;

export type CliNamespaceCleanup =
  | { readonly state: "pending" | "running" }
  | { readonly state: "finished"; readonly volumes: readonly string[] }
  | { readonly state: "ended"; readonly code: CleanupEndCode; readonly message: string };

/** One clean in the caller's Organization as the CLI polls it; null when there is none. */
export const readCliNamespaceCleanup = Effect.fn("NamespaceCleanup.readCli")(function* (organizationId: string, cleanupId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select().from(namespaceCleanup)
    .where(and(eq(namespaceCleanup.organizationId, organizationId), eq(namespaceCleanup.id, cleanupId)));
  if (row === undefined) return null;
  switch (row.state) {
    case "pending":
    case "running":
      return { state: row.state } satisfies CliNamespaceCleanup;
    case "finished":
      return { state: "finished", volumes: row.volumes ?? [] } satisfies CliNamespaceCleanup;
    default: {
      const code = row.endCode ?? "lost";
      const message = code === "refused" || code === "incomplete" ? row.endMessage ?? "" : CLI_END_MESSAGES[code];
      return { state: "ended", code, message } satisfies CliNamespaceCleanup;
    }
  }
});
