import "@tanstack/react-start/server-only";
import { Layer, ManagedRuntime } from "effect";
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

const SHUTDOWN_TIMEOUT_MS = 10_000;

// The production server has no graceful-shutdown hook of its own, so release
// the runtime's resources (database pool, open machine sessions) on SIGTERM
// before exiting. Development runs under Vite, which owns process signals.
if (import.meta.env.PROD) {
  process.once("SIGTERM", () => {
    const timeout = setTimeout(() => process.exit(1), SHUTDOWN_TIMEOUT_MS);
    void AppRuntime.dispose().then(
      () => {
        clearTimeout(timeout);
        process.exit(0);
      },
      (cause: unknown) => {
        clearTimeout(timeout);
        console.error("Runtime shutdown failed.", cause);
        process.exit(1);
      },
    );
  });
}
