import "@tanstack/react-start/server-only";
import type { MachineId } from "@ployz/sdk";
import {
  Context,
  Effect,
  Layer,
  type Scope,
} from "effect";
import { orderDialEntries } from "#/modules/runtime/dial-entry";
import {
  Ployz,
  type PloyzProviderError,
  type PloyzSession,
} from "#/modules/runtime/ployz.server";
import {
  EnrollmentRelay,
  loadOrganizationDialTenant,
} from "#/modules/machines/enrollment.server";
import type { OrganizationDialAccess } from "#/modules/machines/enrollment";
import { Database } from "#/server/database.server";
import { AppConfig } from "#/server/config.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

export type ConnectedRuntimeClient = PloyzSession;

export type ScopedRuntimeClientSession =
  | { readonly status: "connected"; readonly connected: ConnectedRuntimeClient }
  | { readonly status: "no_connection" }
  | {
      readonly status: "unreachable";
      readonly error: PloyzProviderError | null;
    };

export interface OrganizationRuntimeService {
  readonly open: (
    organizationId: string,
  ) => Effect.Effect<ScopedRuntimeClientSession, Error, Scope.Scope>;
}

export class OrganizationRuntime extends Context.Service<
  OrganizationRuntime,
  OrganizationRuntimeService
>()("ployz/OrganizationRuntime") {}

type LoadDialTenant = (
  organizationId: string,
) => Effect.Effect<OrganizationDialAccess, Error>;

export function makeOrganizationRuntimeLayer(loadDialTenant: LoadDialTenant) {
  return Layer.effect(
    OrganizationRuntime,
    Effect.gen(function* () {
      const ployz = yield* Ployz;
      return {
        open: Effect.fn("OrganizationRuntime.open")(function* (
          organizationId: string,
        ) {
          const access = yield* loadDialTenant(organizationId);
          switch (access.kind) {
            case "missing":
              return { status: "no_connection" as const };
            case "unreachable":
              return { status: "unreachable" as const, error: null };
            case "ready":
              break;
            default: {
              const exhaustive: never = access;
              return exhaustive;
            }
          }

          const attempts = orderDialEntries(access.tenant).map((machineId) =>
            ployz
              .connect({
                relayUrl: access.tenant.relayUrl,
                bearer: access.tenant.bearer,
                pairing: access.tenant.pairing,
                // SAFETY: Cloud and the SDK use the same machine identifier bytes.
                machineId: machineId as MachineId,
              })
              .pipe(
                Effect.map((connected) => ({
                  status: "connected" as const,
                  connected,
                })),
              ),
          );
          if (attempts.length === 0) {
            return { status: "unreachable" as const, error: null };
          }
          return yield* Effect.firstSuccessOf(attempts).pipe(
            Effect.catch((error) =>
              Effect.succeed({
                status: "unreachable" as const,
                error,
              }),
            ),
          );
        }),
      } satisfies OrganizationRuntimeService;
    }),
  );
}

export const OrganizationRuntimeLive = Layer.unwrap(
  Effect.gen(function* () {
    const database = yield* Database;
    const enrollmentRelay = yield* EnrollmentRelay;
    const config = yield* AppConfig;
    const encryption = yield* SecretEncryption;
    return makeOrganizationRuntimeLayer((organizationId) =>
      loadOrganizationDialTenant(organizationId).pipe(
        Effect.provideService(Database, database),
        Effect.provideService(EnrollmentRelay, enrollmentRelay),
        Effect.provideService(AppConfig, config),
        Effect.provideService(SecretEncryption, encryption),
      ),
    );
  }),
);
