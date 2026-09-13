import "@tanstack/react-start/server-only";
import type { Connection, MachineId } from "@ployz/sdk";
import {
  Context,
  Deferred,
  Effect,
  Exit,
  Layer,
  Scope,
  Schema,
  Stream,
} from "effect";
import {
  Ployz,
  type PloyzProviderError,
  type PloyzSession,
} from "#/modules/runtime/ployz.server";
import {
  loadOrganizationConnections,
} from "#/modules/machines/connections.server";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

export const PAIRING_REMOVAL_CHANNEL = "ployz_pairing_removed";

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

export function makeOrganizationRuntimeLayer(
  loadConnections: LoadConnections,
  subscribe: Effect.Effect<Stream.Stream<string, Error>, Error, Scope.Scope> =
    Effect.succeed(Stream.never),
) {
  return Layer.effect(
    OrganizationRuntime,
    Effect.gen(function* () {
      const ployz = yield* Ployz;
      const removals = yield* subscribe;
      type Session = {
        scope: Scope.Closeable;
        generation?: string;
        removed: Set<string>;
        closed: boolean;
      };
      const sessions = new Map<string, Set<Session>>();
      let listenerFailure: Error | undefined;
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
      const removalSchema = Schema.fromJsonString(Schema.Struct({
        organizationId: Schema.String,
        generation: Schema.String,
      }));
      yield* Stream.runForEach(removals, (payload) => Schema.decodeUnknownEffect(removalSchema)(payload).pipe(
        Effect.flatMap(({ organizationId, generation }) => cancel(organizationId, generation)),
      )).pipe(
        Effect.ensuring(Effect.gen(function* () {
          listenerFailure = new Error("Pairing removal listener is unavailable");
          yield* Effect.forEach([...sessions.values()].flatMap((active) => [...active]), close, {
            concurrency: "unbounded",
            discard: true,
          });
        })),
        Effect.catch((error) => Effect.logError("Pairing removal listener failed", error)),
        Effect.forkScoped,
      );
      return {
        cancel,
        open: Effect.fn("OrganizationRuntime.open")(function* (organizationId: string, machineId?: MachineId) {
          if (listenerFailure) return yield* Effect.fail(listenerFailure);
          const scope = yield* Scope.fork(yield* Effect.scope);
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
            if (listenerFailure) return yield* Effect.fail(listenerFailure);
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
      database.subscribe(PAIRING_REMOVAL_CHANNEL),
    );
  }),
);
