import "@tanstack/react-start/server-only";
import { Layer, ManagedRuntime } from "effect";
import { EnrollmentRelayLive } from "#/modules/machines/enrollment.server";
import { OrganizationRuntimeLive } from "#/modules/runtime/organization-runtime.server";
import { PloyzLive } from "#/modules/runtime/ployz.server";
import { PolarLive } from "#/modules/billing/polar-provider.server";
import { GithubApiLive } from "#/modules/github/github-observation.api";
import { InngestLive } from "#/modules/inngest/client";
import { AuthLive } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { DatabaseLive } from "#/server/database.server";
import { SecretEncryptionLive } from "#/utils/encrypted-secret.server";

const InfrastructureLive = Layer.mergeAll(
  SecretEncryptionLive,
  PolarLive,
  InngestLive,
  EnrollmentRelayLive,
  GithubApiLive,
).pipe(
  Layer.provideMerge(DatabaseLive),
  Layer.provideMerge(PloyzLive),
  Layer.provideMerge(AppConfig.layer),
);

const RuntimeLive = OrganizationRuntimeLive.pipe(
  Layer.provideMerge(InfrastructureLive),
);

export const AppLive = AuthLive.pipe(
  Layer.provideMerge(InfrastructureLive),
  Layer.merge(RuntimeLive),
);

export type AppServices = Layer.Success<typeof AppLive>;

export const AppRuntime = ManagedRuntime.make(AppLive);
