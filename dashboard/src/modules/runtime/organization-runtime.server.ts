import "@tanstack/react-start/server-only";
import type { Connection, MachineId } from "@ployz/sdk";
import { Context, Deferred, Effect, Exit, Layer, Schedule, Scope } from "effect";
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

/**
 * Ceiling on establishing a shared organization session. The SDK's own
 * `timeoutMs` bounds the whole session lifetime, which would cut long deploys,
 * so only the connect phase is bounded here. Interruption propagates to the
 * SDK through its AbortSignal.
 */
export const ORGANIZATION_CONNECT_TIMEOUT = "30 seconds";

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
          cursor = changes.cursor;
          if (!changes.changed) return;
          const access = yield* loadConnections(organizationId);
          if (access.kind === "missing" || access.generation !== session.generation) yield* close(session);
        }).pipe(
          // Closing the caller's scope interrupts this watcher. Interrupting a pooled query makes the
          // SQL client send pg_cancel_backend later, which can cancel whatever statement that
          // connection runs next; so a check finishes, and only the sleep between checks is interrupted.
          Effect.uninterruptible,
          Effect.repeat({ schedule: Schedule.spaced(PAIRING_CHANGE_POLL), while: () => !session.closed }),
          // Removals are unobservable until the log is readable again, so fail closed.
          Effect.catch((error) => Effect.logWarning("Pairing change check failed; closing the session.", error).pipe(
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
              // Forked outside the session scope so the check can close that scope.
              Effect.tap(() => Effect.forkIn(watchPairing(organizationId, session, cursor), parent)),
              Effect.map((connected) => session.closed ? noConnection : ({
                status: "connected" as const,
                connected,
              })),
              Effect.catch((error) => Effect.succeed(session.closed ? noConnection : ({
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
