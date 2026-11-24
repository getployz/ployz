import "@tanstack/react-start/server-only";
import { and, eq, lt, sql } from "drizzle-orm";
import { Data, Effect } from "effect";
import { SERVERS_UNANSWERED_MESSAGE } from "#/lib/refusal";
import type { Actor } from "#/modules/identity/actor";
import { ownedNamespaces } from "#/modules/config-store/store-namespaces.server";
import { type NamespaceCleanupRequestedEventData, namespaceCleanupRequested } from "#/modules/machines/machines.events";
import { CLEANUP_PENDING_LIMIT_MS, CLEANUP_RUNNING_LIMIT_MS } from "#/modules/machines/namespace-cleanup";
import {
  CLEANUP_END_CODES,
  CLEANUP_END_WORDS,
  type CleanupEndCode,
  type CleanupEndCodeWithoutMessage,
  type CleanupState,
} from "#/modules/machines/namespace-cleanup-states";
import { namespaceCleanup } from "#/modules/machines/tables";
import { attemptRows, ready, settled, within } from "#/modules/inngest/attempt-row.server";
import { enqueueEvent } from "#/modules/inngest/outbox.server";
import { requireAccess } from "#/modules/organization/access.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import { Database } from "#/server/database.server";
import { Conflict } from "#/server/public-error";
import { SYSTEM_NAMESPACE } from "./server-services";
import { checkedNow } from "#/db/tables";

/** Which of `namespaces`, seen on the Organization's Servers, no Environment owns: the Servers page offers to remove them. */
export const listStrayNamespaces = Effect.fn("NamespaceCleanup.strays")(function* (
  actor: Actor, input: { organizationSlug: string; namespaces: readonly string[] },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "servers:read");
  const owned = new Set(yield* ownedNamespaces(organization.id));
  return input.namespaces.filter((namespace) => namespace !== SYSTEM_NAMESPACE && !owned.has(namespace));
});

/**
 * The Organization's Servers, for removing `namespace`: only one no Environment owns, and never Ployz's own. An owned
 * one goes with its Environment (Settings › Delete), which the Store records.
 */
const unownedSession = Effect.fn("NamespaceCleanup.session")(function* (actor: Actor, organizationSlug: string, namespace: string) {
  const { organization } = yield* requireAccess(actor, organizationSlug, "servers:operate");
  if (namespace === SYSTEM_NAMESPACE || (yield* ownedNamespaces(organization.id)).includes(namespace)) {
    return yield* new Conflict({ userFacing: true, message: `${namespace} belongs to an Environment: delete that Environment in its Settings instead.` });
  }
  const session = yield* (yield* OrganizationRuntime).open(organization.id);
  if (session.status !== "connected") return yield* new Conflict({ userFacing: true, message: SERVERS_UNANSWERED_MESSAGE });
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
  actor: Actor,
  input: { organizationSlug: string; namespace: string; confirmDataLoss: readonly DataLossIdentity[] },
) {
  const session = yield* unownedSession(actor, input.organizationSlug, input.namespace);
  const outcome = yield* session.destroyNamespace(input.namespace, { confirmed: [...input.confirmDataLoss] }).pipe(
    Effect.map((done) => done.type === "success" ? { removed: true as const } : { failed: "Some servers didn't finish removing it. Try again." }),
    Effect.catchTag("MissingDataLossIdentities", (missing) => Effect.succeed({ missing: missing.identities })),
    Effect.mapError(() => new Conflict({ userFacing: true, message: "Your servers didn't answer. Try again." })),
  );
  return outcome;
}, Effect.scoped);

class NamespaceCleanupUnreachable extends Data.TaggedError("NamespaceCleanupUnreachable")<{
  readonly cause: unknown;
}> {}

type CleanupRow = typeof namespaceCleanup.$inferSelect;
type CleanupRequest = NamespaceCleanupRequestedEventData;
/** What a run step answers with: the row's state once the step is done with it. */
type CleanupRunReply = { readonly cleanupId: string; readonly state: CleanupState };

const rows = attemptRows(namespaceCleanup, "The namespace clean was not found.");

const ended = (code: CleanupEndCodeWithoutMessage) =>
  ({ state: CLEANUP_END_CODES[code], endCode: code, endMessage: null, endedAt: sql`now()` }) as const;
const endedSaying = (code: "refused" | "incomplete", message: string) =>
  ({ state: CLEANUP_END_CODES[code], endCode: code, endMessage: within(message), endedAt: sql`now()` }) as const;

/** Asks for the row's run in the caller's transaction; the outbox sends it once that commits. */
const enqueueCleanup = (row: CleanupRow) =>
  enqueueEvent(namespaceCleanupRequested.create({ cleanupId: row.id, organizationId: row.organizationId }));

/**
 * A clean the signed-in CLI asked for, deleting exactly the Volumes it previewed, under the attempt its request admits.
 * Answers the clean already active on the Namespace instead of a second.
 */
export const requestNamespaceCleanup = Effect.fn("NamespaceCleanup.request")(function* (
  caller: { readonly organizationId: string; readonly userId: string },
  input: {
    readonly attemptId: string;
    readonly namespace: string;
    readonly confirmDataLoss: readonly DataLossIdentity[];
  },
) {
  const { drizzle } = yield* Database;
  const [inserted] = yield* drizzle.insert(namespaceCleanup).values({
    id: input.attemptId,
    organizationId: caller.organizationId,
    namespace: input.namespace,
    requestedByUserId: caller.userId,
    confirmDataLoss: [...input.confirmDataLoss],
  }).onConflictDoNothing().returning();
  if (inserted !== undefined) {
    yield* enqueueCleanup(inserted);
    return inserted.id;
  }
  const [active] = yield* drizzle.select().from(namespaceCleanup).where(and(
    eq(namespaceCleanup.organizationId, caller.organizationId),
    eq(namespaceCleanup.namespace, input.namespace),
    rows.active,
  )).limit(1);
  if (active === undefined) return yield* new Conflict({ userFacing: true, message: "A clean of this namespace just ended. Try again." });
  return active.id;
});

export const bindCleanupRun = (request: CleanupRequest, runId: string) =>
  rows.bind(request.cleanupId, runId, [eq(namespaceCleanup.organizationId, request.organizationId)]);

export const executeCleanupOnce = Effect.fn("NamespaceCleanup.execute")(function* (request: CleanupRequest, runId: string) {
  const { drizzle } = yield* Database;
  const id = request.cleanupId;
  const state = yield* rows.executeOnce({
    id,
    runId,
    lost: ended("lost"),
    endsFrom: ["running", "unknown"],
    prepare: Effect.gen(function* () {
      const [row] = yield* drizzle.select({ namespace: namespaceCleanup.namespace, confirmDataLoss: namespaceCleanup.confirmDataLoss })
        .from(namespaceCleanup).where(eq(namespaceCleanup.id, id));
      if (row === undefined) return settled(yield* rows.stateOf(id));
      const session = yield* (yield* OrganizationRuntime).open(request.organizationId);
      if (session.status !== "connected") return yield* new NamespaceCleanupUnreachable({ cause: session.status });
      if (row.namespace === SYSTEM_NAMESPACE || (yield* ownedNamespaces(request.organizationId)).includes(row.namespace)) {
        const refused = yield* rows.end(
          id, runId, endedSaying("refused", `${row.namespace} belongs to an Environment now. Nothing was removed.`), ["pending"],
        );
        return settled(refused ?? (yield* rows.stateOf(id)));
      }
      return ready({ row, connected: session.connected });
    }),
    call: ({ row, connected }) => connected.destroyNamespace(row.namespace, { confirmed: row.confirmDataLoss }).pipe(
      Effect.map((outcome) => outcome.type === "success"
        ? ({ state: "finished", volumes: row.confirmDataLoss.map(({ id }) => id.name).sort(), endedAt: sql`now()`, endCode: null, endMessage: null }) as const
        : endedSaying("incomplete", "Some servers didn't finish removing the namespace.")),
      Effect.catchTag("MissingDataLossIdentities", () =>
        Effect.succeed(endedSaying("refused", "The servers hold Volumes this clean didn't confirm. Nothing was removed."))),
      Effect.catch(() => Effect.succeed(ended("lost"))),
      Effect.timeoutOrElse({ duration: CLEANUP_RUNNING_LIMIT_MS, orElse: () => Effect.succeed(ended("lost")) }),
    ),
  });
  return { cleanupId: id, state } satisfies CleanupRunReply;
}, Effect.scoped);

const RUN_ENDS = {
  failure: { pending: "not_started", running: "lost" },
  cancellation: { pending: "cancelled", running: "interrupted" },
} as const satisfies Record<string, Record<"pending" | "running", CleanupEndCodeWithoutMessage>>;

/**
 * A run that failed for good or was cancelled must not leave its row active. A run cancelled before it bound its row
 * names the row by `cleanupId` from its event. How many rows this call ended.
 */
export const closeCleanupRun = (runId: string, reason: keyof typeof RUN_ENDS, cleanupId?: string) => {
  const { pending, running } = RUN_ENDS[reason];
  return rows.closeRun(runId, cleanupId, (whenPending) => ({
    state: whenPending(CLEANUP_END_CODES[pending], CLEANUP_END_CODES[running]),
    endCode: whenPending(pending, running),
  }));
};

/** The hourly sweep: a request no run picked up in time never starts; a clean running this long lost its run. */
export const closeStaleCleanups = Effect.fn("NamespaceCleanup.closeStale")(function* () {
  const { running, pending } = yield* rows.closeStale({
    running: { limitMs: CLEANUP_RUNNING_LIMIT_MS, end: ended("lost") },
    pending: {
      end: ended("not_started"),
      when: lt(namespaceCleanup.requestedAt, sql`${checkedNow()} - make_interval(secs => ${CLEANUP_PENDING_LIMIT_MS / 1000})`),
    },
  });
  return { failed: pending, unknown: running };
});

export type CliNamespaceCleanup =
  | { readonly state: "pending" | "running" }
  | { readonly state: "finished"; readonly volumes: readonly string[] }
  | { readonly state: "ended"; readonly code: CleanupEndCode; readonly message: string };

/** Why a clean ended without finishing: the Engine's words when it gave some, else Cloud's. */
export const cleanupEndMessage = (row: Pick<CleanupRow, "endCode" | "endMessage">) =>
  row.endCode === null || row.endCode === "refused" || row.endCode === "incomplete" ? row.endMessage : CLEANUP_END_WORDS[row.endCode];

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
      const message = cleanupEndMessage({ endCode: code, endMessage: row.endMessage }) ?? "";
      return { state: "ended", code, message } satisfies CliNamespaceCleanup;
    }
  }
});
