import "@tanstack/react-start/server-only";
import type { Connection, MachineId, RuntimeWatchView } from "@ployz/sdk";
import { Clock, Context, Data, Deferred, Effect, Exit, FiberSet, Layer, Schedule, Scope } from "effect";
import {
  Ployz,
  PloyzProviderError,
  type PloyzSession,
} from "#/modules/runtime/ployz.server";
import {
  loadOrganizationConnections,
} from "#/modules/machines/connections.server";
import { runtimeFrameRecord } from "#/modules/runtime/runtime-frame-record.server";
import { currentChangeCursor, readChangeWindow, type OrganizationChangeLogFailure } from "#/modules/organization/change-log.server";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

/**
 * How often a connected session reads its Organization's change log for pairing changes.
 * ponytail: one poll per session; move to one loop per instance when sessions reach the thousands.
 */
export const PAIRING_CHANGE_POLL = "1 second";
/** Consecutive failed pairing checks, one per poll, before a session closes because removals can't be seen. */
export const PAIRING_CHECK_FAILURE_LIMIT = 30;

/**
 * Ceiling on establishing a shared organization session. The SDK's own
 * `timeoutMs` bounds the whole session lifetime, which would cut long deploys,
 * so only the connect phase is bounded here. Interruption propagates to the
 * SDK through its AbortSignal.
 */
export const ORGANIZATION_CONNECT_TIMEOUT = "30 seconds";

/** How long Cloud waits for a Cluster's first Runtime Watch frame when it reads the Cluster once. */
export const RUNTIME_FRAME_TIMEOUT_MS = 10_000;

/**
 * How long a plan may spend reading the Cluster, all its reads together. The CLI gives the whole request 60 seconds,
 * and a plan that opens two sessions could otherwise spend a connect and a frame ceiling on each.
 */
export const PLAN_CLUSTER_BUDGET_MS = 45_000;

/**
 * The Cluster time an effect may still spend: it runs only while one of its Cluster reads is in flight, so database
 * and Store reads between them spend none of it. Null for a run step or a page, whose reads keep only their own ceilings.
 */
type ClusterTime = { readonly budgetMs: number; spentMs: number; reading: number; since: number };

export const ClusterBudget = Context.Reference<ClusterTime | null>("ployz/ClusterBudget", { defaultValue: () => null });

/**
 * The Cluster time a short-lived session gets to close when its caller reads the Cluster under its own ceilings. Past
 * it, the connection is aborted and the close goes on in the background.
 */
export const SESSION_CLOSE_CLUSTER_MS = 2_000;

/** A read found the Cluster budget spent, or the budget ran out while it waited: `inFlight`, so what it asked may have landed. */
export class ClusterOutOfTime extends Data.TaggedError("ClusterOutOfTime")<{ readonly inFlight: boolean }> {}

/** `self` with `budgetMs` of Cluster time across all its Cluster reads. */
export const withClusterBudget = (budgetMs: number) => <A, E, R>(self: Effect.Effect<A, E, R>) =>
  Effect.suspend(() => self.pipe(Effect.provideService(ClusterBudget, { budgetMs, spentMs: 0, reading: 0, since: 0 })));

const timeLeft = (time: ClusterTime, now: number) => time.budgetMs - time.spentMs - (time.reading > 0 ? now - time.since : 0);

/** Fails when the Cluster time is already spent, so an effect that leads up to a Cluster read does none of its work first. */
export const requireClusterTime = Effect.gen(function* () {
  const time = yield* ClusterBudget;
  if (time !== null && timeLeft(time, yield* Clock.currentTimeMillis) <= 0) return yield* new ClusterOutOfTime({ inFlight: false });
});

/**
 * One Cluster read, cut on top of its own ceiling when its budget runs out. Only an SDK call goes through here, never a
 * pooled SQL query: interrupting one makes the SQL client send pg_cancel_backend later, which can cancel whatever
 * statement that connection runs next.
 */
export const clusterRead = <A, E, R>(read: Effect.Effect<A, E, R>): Effect.Effect<A, E | ClusterOutOfTime, R> =>
  Effect.gen(function* () {
    const time = yield* ClusterBudget;
    if (time === null) return yield* read;
    const now = yield* Clock.currentTimeMillis;
    const left = timeLeft(time, now);
    if (left <= 0) return yield* new ClusterOutOfTime({ inFlight: false });
    if (time.reading === 0) time.since = now;
    time.reading += 1;
    return yield* read.pipe(
      Effect.timeoutOrElse({ duration: left, orElse: () => Effect.fail(new ClusterOutOfTime({ inFlight: true })) }),
      Effect.ensuring(Effect.map(Clock.currentTimeMillis, (end) => {
        time.reading -= 1;
        if (time.reading === 0) time.spentMs += end - time.since;
      })),
    );
  });

export type ConnectedRuntimeClient = PloyzSession;

export type ScopedRuntimeClientSession =
  | { readonly status: "connected"; readonly connected: ConnectedRuntimeClient }
  | { readonly status: "no_connection" }
  | {
      readonly status: "unreachable";
      readonly error: PloyzProviderError | null;
    };

/** Which paired Servers a session may enter by: only one, or any but one. Without it, any. */
export type RuntimeEntry = { readonly only: MachineId } | { readonly avoiding: MachineId };

export interface OrganizationRuntimeService {
  readonly cancel: (organizationId: string, generation: string) => Effect.Effect<void>;
  readonly open: (
    organizationId: string,
    entry?: RuntimeEntry,
  ) => Effect.Effect<ScopedRuntimeClientSession, Error, Scope.Scope>;
}

export class OrganizationRuntime extends Context.Service<
  OrganizationRuntime,
  OrganizationRuntimeService
>()("ployz/OrganizationRuntime") {}

type LoadConnections = (
  organizationId: string,
) => Effect.Effect<
  { readonly kind: "missing" } | { readonly kind: "ready"; readonly generation: string; readonly connections: readonly Connection[] },
  Error
>;

/** The change log as a session watches its pairing: where to start, and whether `organization_pairing` changed since. */
export type PairingChanges = {
  readonly current: Effect.Effect<string, OrganizationChangeLogFailure>;
  readonly changedSince: (
    organizationId: string,
    since: string,
  ) => Effect.Effect<{ readonly cursor: string; readonly changed: boolean }, OrganizationChangeLogFailure>;
};

/**
 * What Cloud keeps from a Runtime Watch frame its Organization's session read. `digest` covers every field `write`
 * reads. `write` settles the frame, or asks a job to finish what it can't observe, as an Inngest send does.
 */
export type FrameRecord = {
  readonly digest: (frame: RuntimeWatchView) => string;
  readonly write: (organizationId: string, frame: RuntimeWatchView) => Effect.Effect<"settled" | "asked", Error>;
};

export const RETRY_AFTER_MS = 60_000;

const settledByName: FrameRecord = {
  digest: (frame) => JSON.stringify(frame.machines.map(({ machine }) => [machine.id, machine.name])),
  write: () => Effect.succeed("settled"),
};

/**
 * Hands each frame to `record.write` off the read that carried it: the read never waits on the write, so no deadline
 * of the read's reaches the SQL. Each Organization writes in one fiber of the layer's scope at a time; frames read
 * during a write coalesce into the latest, written next. A frame whose digest equals the last settled one is skipped,
 * and one equal to the last asked one is skipped for `RETRY_AFTER_MS`, then written again in case the job never
 * finished. A failed write is logged and retried by the next frame. When the layer closes, the write in flight
 * finishes and a coalesced one is dropped, for the Cluster's next frame to write.
 */
const makeFrameRecorder = Effect.fnUntraced(function* (record: FrameRecord) {
  const run = yield* FiberSet.makeRuntime<never>();
  const clock = yield* Clock.Clock;
  type Pending = { readonly digest: string; readonly frame: RuntimeWatchView };
  type Written = { readonly digest: string; readonly until: number };
  type Organization = { written?: Written; next?: Pending; writing: boolean };
  const organizations = new Map<string, Organization>();
  const current = (organization: Organization, digest: string) =>
    organization.written?.digest === digest && clock.currentTimeMillisUnsafe() < organization.written.until;
  const writeAll = (organizationId: string, organization: Organization) => {
    // Clears `writing` in the same step that finds nothing left, so a frame read meanwhile starts a new write.
    const take = Effect.sync(() => {
      const next = organization.next;
      organization.next = undefined;
      if (next === undefined) organization.writing = false;
      return next;
    });
    return Effect.gen(function* () {
      for (let next = yield* take; next !== undefined; next = yield* take) {
        if (current(organization, next.digest)) continue;
        const outcome = yield* record.write(organizationId, next.frame).pipe(
          Effect.catchCause((cause) => Effect.logWarning("Runtime frame not recorded", cause).pipe(Effect.as("failed" as const))),
          // Uninterruptible like the pairing check, since interrupting pooled SQL can cancel a later statement.
          Effect.uninterruptible,
        );
        // Advanced only once the write lands, so a failed write is retried by the next frame.
        if (outcome === "settled") organization.written = { digest: next.digest, until: Infinity };
        if (outcome === "asked") organization.written = { digest: next.digest, until: clock.currentTimeMillisUnsafe() + RETRY_AFTER_MS };
      }
    });
  };
  return (organizationId: string, frame: RuntimeWatchView) => {
    const digest = record.digest(frame);
    const organization = organizations.get(organizationId) ?? { writing: false };
    organizations.set(organizationId, organization);
    if (!organization.writing && current(organization, digest)) return;
    organization.next = { digest, frame };
    if (organization.writing) return;
    organization.writing = true;
    run(writeAll(organizationId, organization));
  };
});

/** `session`, whose reads hand each frame they read to `record`. */
function recordingFrames(session: PloyzSession, record: (frame: RuntimeWatchView) => void): PloyzSession {
  async function* recordEach(frames: AsyncIterable<RuntimeWatchView>) {
    for await (const frame of frames) {
      record(frame);
      yield frame;
    }
  }
  return {
    ...session,
    watch: (options) => session.watch(options).pipe(Effect.map(recordEach)),
    watchFirstFrame: (timeoutMs) => session.watchFirstFrame(timeoutMs).pipe(Effect.tap((frame) => Effect.sync(() => record(frame)))),
  };
}

export function makeOrganizationRuntimeLayer(
  loadConnections: LoadConnections,
  pairingChanges: PairingChanges,
  frameRecord: FrameRecord = settledByName,
) {
  return Layer.effect(
    OrganizationRuntime,
    Effect.gen(function* () {
      const ployz = yield* Ployz;
      const record = yield* makeFrameRecorder(frameRecord);
      type Session = {
        scope: Scope.Closeable;
        generation?: string;
        removed: Set<string>;
        closed: boolean;
      };
      const sessions = new Map<string, Set<Session>>();
      const close = (session: Session) => Effect.suspend(() => {
        session.closed = true;
        return Scope.close(session.scope, Exit.void);
      });
      const cancel = Effect.fn("OrganizationRuntime.cancel")(function* (organizationId: string, generation: string) {
        const active = sessions.get(organizationId);
        if (!active) return;
        yield* Effect.forEach([...active], (session) => {
          session.removed.add(generation);
          return session.generation === generation ? close(session) : Effect.void;
        }, { concurrency: "unbounded", discard: true });
      });
      // Removal disables the pairing (an update) before deleting it, so any pairing change
      // re-checks it; only removal or a new generation closes the session.
      const watchPairing = (organizationId: string, session: Session, since: string) => {
        let cursor = since;
        return Effect.gen(function* () {
          const changes = yield* pairingChanges.changedSince(organizationId, cursor);
          if (changes.changed) {
            const access = yield* loadConnections(organizationId);
            if (access.kind === "missing" || access.generation !== session.generation) yield* close(session);
          }
          // Advanced only once the change is checked, so a retried check still sees it.
          cursor = changes.cursor;
        }).pipe(
          // Closing the caller's scope interrupts this watcher. Interrupting a pooled query makes the
          // SQL client send pg_cancel_backend later, which can cancel whatever statement that
          // connection runs next; so a check finishes, and only the sleep between checks is interrupted.
          Effect.uninterruptible,
          // A brief database hiccup must not close the session and cancel the Drain running through it.
          Effect.retry({
            schedule: Schedule.spaced(PAIRING_CHANGE_POLL),
            times: PAIRING_CHECK_FAILURE_LIMIT - 1,
            while: () => !session.closed,
          }),
          Effect.repeat({ schedule: Schedule.spaced(PAIRING_CHANGE_POLL), while: () => !session.closed }),
          // Removals are unobservable until the log is readable again, so a lasting outage fails closed.
          Effect.catch((error) => Effect.logWarning("Pairing change checks kept failing; closing the session.", error).pipe(
            Effect.andThen(close(session)),
          )),
        );
      };
      return {
        cancel,
        open: Effect.fn("OrganizationRuntime.open")(function* (organizationId: string, entry?: RuntimeEntry) {
          const parent = yield* Effect.scope;
          const scope = yield* Scope.fork(parent);
          const cancelled = yield* Deferred.make<void>();
          const session: Session = { scope, removed: new Set(), closed: false };
          const scopes = sessions.get(organizationId) ?? new Set<Session>();
          scopes.add(session);
          sessions.set(organizationId, scopes);
          yield* Scope.addFinalizer(scope, Effect.gen(function* () {
            session.closed = true;
            scopes.delete(session);
            if (sessions.get(organizationId) === scopes && scopes.size === 0) {
              sessions.delete(organizationId);
            }
            yield* Deferred.succeed(cancelled, undefined);
          }));
          const noConnection = { status: "no_connection" as const };
          return yield* Effect.gen(function* () {
            // Taken before loading, so a change committed while loading is still seen.
            // Uninterruptible for the same reason as the watcher: the cancel race below can interrupt it.
            const cursor = yield* Effect.uninterruptible(pairingChanges.current);
            const access = yield* loadConnections(organizationId);
            if (session.closed || access.kind === "missing") return noConnection;
            session.generation = access.generation;
            if (session.removed.has(access.generation)) {
              yield* close(session);
              return noConnection;
            }
            const connections = entry === undefined
              ? access.connections
              : access.connections.filter((connection) => "only" in entry
                ? connection.machine_id === entry.only
                : connection.machine_id !== entry.avoiding);
            if (entry !== undefined && connections.length === 0) return noConnection;
            if (connections.length === 0) {
              return { status: "unreachable" as const, error: null };
            }
            // A close spends the caller's Cluster budget; a long-lived session, opened with none, waits for its close.
            return yield* ployz.connect({ connections }, { closeWithin: clusterRead }).pipe(
              Effect.timeoutOrElse({
                duration: ORGANIZATION_CONNECT_TIMEOUT,
                orElse: () => Effect.fail(new PloyzProviderError({
                  operation: "connect",
                  cause: new Error("Connecting to the organization's machines timed out"),
                })),
              }),
              clusterRead,
              // Forked outside the session scope so the check can close that scope.
              Effect.tap(() => Effect.forkIn(watchPairing(organizationId, session, cursor), parent)),
              Effect.map((connected) => session.closed ? noConnection : ({
                status: "connected" as const,
                connected: recordingFrames(connected, (frame) => record(organizationId, frame)),
              })),
              // Out of Cluster time fails the open, so its plan refuses instead of reading on as if unpaired.
              Effect.catch((error) => error instanceof ClusterOutOfTime ? Effect.fail(error) : Effect.succeed(session.closed ? noConnection : ({
                status: "unreachable" as const,
                error,
              }))),
            );
          }).pipe(
            Effect.provideService(Scope.Scope, scope),
            Effect.raceFirst(Deferred.await(cancelled).pipe(Effect.as(noConnection))),
            Effect.onError(() => Scope.close(scope, Exit.void)),
          );
        }),
      } satisfies OrganizationRuntimeService;
    }),
  );
}

export const OrganizationRuntimeLive = Layer.unwrap(
  Effect.gen(function* () {
    const database = yield* Database;
    const encryption = yield* SecretEncryption;
    return makeOrganizationRuntimeLayer((organizationId) =>
      loadOrganizationConnections(organizationId).pipe(
        Effect.provideService(Database, database),
        Effect.provideService(SecretEncryption, encryption),
      ),
      {
        current: currentChangeCursor().pipe(Effect.provideService(Database, database)),
        changedSince: (organizationId, since) => readChangeWindow({
          organizationId, since, sourceTables: ["organization_pairing"],
        }).pipe(
          Effect.map((window) => ({ cursor: window.cursor, changed: window.kind === "full" || window.sourceTables.length > 0 })),
          Effect.provideService(Database, database),
        ),
      },
      yield* runtimeFrameRecord,
    );
  }),
);

/**
 * One Runtime Watch frame of the Organization's Cluster, for an operation that refuses without one: null when no
 * Cluster is paired. A fresh connection through the relay can take longer than a glance allows, so only the connect
 * phase and the frame are bounded, each by its own ceiling.
 */
export const operationRuntimeFrame = Effect.fn("OrganizationRuntime.operationFrame")(function* (organizationId: string) {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status !== "connected") return null;
  return yield* session.connected.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS).pipe(clusterRead);
}, Effect.scoped);

/**
 * One Runtime Watch frame of the Organization's Cluster, for a read that looks once: null when no Cluster is paired.
 * Fails when the Cluster doesn't answer within 5 seconds of Cluster time, so a slow one never holds up what asked.
 */
export const firstRuntimeFrame = Effect.fn("OrganizationRuntime.firstFrame")(function* (organizationId: string) {
  return yield* operationRuntimeFrame(organizationId);
}, withClusterBudget(5_000));
