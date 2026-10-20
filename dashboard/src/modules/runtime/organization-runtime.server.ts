import "@tanstack/react-start/server-only";
import type { Connection, MachineId } from "@ployz/sdk";
import { Clock, Context, Data, Deferred, Effect, Exit, Layer, Schedule, Scope } from "effect";
import {
  Ployz,
  PloyzProviderError,
  type PloyzSession,
} from "#/modules/runtime/ployz.server";
import {
  loadOrganizationConnections,
} from "#/modules/machines/connections.server";
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
 * How long into a CLI call its Cluster reads may run. The CLI waits 60 seconds for an operation's answer
 * (`OPERATION_TIMEOUT` in core's cloud_account.rs); ending the reads 10 seconds short leaves the rest for writing what
 * was planned and answering.
 */
export const CLI_CLUSTER_DEADLINE_MS = 50_000;

/** A Cluster read with less than this left before its request's deadline is refused without being started. */
export const CLUSTER_READ_FLOOR_MS = 1_000;

/**
 * When the request reading the Cluster must have its answer, in Clock milliseconds: a CLI call's, or null for a run
 * step or a page, whose reads keep only their own ceilings.
 */
export const ClusterDeadline = Context.Reference<number | null>("ployz/ClusterDeadline", { defaultValue: () => null });

/** The request's deadline cut a Cluster read, or left too little time to start one. */
export class ClusterOutOfTime extends Data.TaggedError("ClusterOutOfTime") {}

/** `self` with its Cluster reads ending `ms` from now, or sooner when an outer deadline ends first. */
export const withClusterDeadline = (ms: number) => <A, E, R>(self: Effect.Effect<A, E, R>) =>
  Effect.gen(function* () {
    const at = (yield* Clock.currentTimeMillis) + ms;
    const outer = yield* ClusterDeadline;
    return yield* self.pipe(Effect.provideService(ClusterDeadline, outer === null ? at : Math.min(outer, at)));
  });

/**
 * One Cluster read, cut at its request's deadline on top of its own ceiling, and refused unstarted when too little time
 * is left. Only Cluster I/O goes through here, never a pooled SQL query: interrupting one makes the SQL client send
 * pg_cancel_backend later, which can cancel whatever statement that connection runs next.
 */
export const clusterRead = <A, E, R>(read: Effect.Effect<A, E, R>) => Effect.gen(function* () {
  const deadline = yield* ClusterDeadline;
  if (deadline === null) return yield* read;
  const left = deadline - (yield* Clock.currentTimeMillis);
  if (left < CLUSTER_READ_FLOOR_MS) return yield* new ClusterOutOfTime();
  return yield* read.pipe(Effect.timeoutOrElse({ duration: left, orElse: () => Effect.fail(new ClusterOutOfTime()) }));
});

export type ConnectedRuntimeClient = PloyzSession;

export type ScopedRuntimeClientSession =
  | { readonly status: "connected"; readonly connected: ConnectedRuntimeClient }
  | { readonly status: "no_connection" }
  | {
      readonly status: "unreachable";
      readonly error: PloyzProviderError | null;
    };

export interface OrganizationRuntimeService {
  readonly cancel: (organizationId: string, generation: string) => Effect.Effect<void>;
  readonly open: (
    organizationId: string,
    machineId?: MachineId,
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

export function makeOrganizationRuntimeLayer(
  loadConnections: LoadConnections,
  pairingChanges: PairingChanges,
) {
  return Layer.effect(
    OrganizationRuntime,
    Effect.gen(function* () {
      const ployz = yield* Ployz;
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
        open: Effect.fn("OrganizationRuntime.open")(function* (organizationId: string, machineId?: MachineId) {
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
          // Loading reads the database, so only the connect below runs under the request's deadline.
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
            const connections = machineId === undefined
              ? access.connections
              : access.connections.filter((connection) => connection.machine_id === machineId);
            if (machineId !== undefined && connections.length === 0) return noConnection;
            if (connections.length === 0) {
              return { status: "unreachable" as const, error: null };
            }
            return yield* ployz.connect({ connections }).pipe(
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
                connected,
              })),
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
    );
  }),
);

/**
 * One Runtime Watch frame of the Organization's Cluster, for an operation that refuses without one: null when no
 * Cluster is paired. A fresh connection through the relay can take longer than a glance allows, so only the connect
 * phase and the frame are bounded, each by its own ceiling and the request's deadline.
 */
export const operationRuntimeFrame = Effect.fn("OrganizationRuntime.operationFrame")(function* (organizationId: string) {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status !== "connected") return null;
  return yield* session.connected.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS).pipe(clusterRead);
}, Effect.scoped);

/**
 * One Runtime Watch frame of the Organization's Cluster, for a read that looks once: null when no Cluster is paired.
 * Fails when the Cluster doesn't answer within 5 seconds, so a slow one never holds up what asked.
 */
export const firstRuntimeFrame = Effect.fn("OrganizationRuntime.firstFrame")(function* (organizationId: string) {
  return yield* operationRuntimeFrame(organizationId);
}, Effect.timeout("5 seconds"));
