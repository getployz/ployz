import "@tanstack/react-start/server-only";
import type { RuntimeWatchView } from "@ployz/sdk";
import { Effect, Exit, Scope } from "effect";
import { authorizeRequest } from "#/modules/organization/access.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzProviderError } from "#/modules/runtime/ployz.server";
import { AppConfig } from "#/server/config.server";

export type OpenedRuntimeWatch =
  | {
      status: "connected";
      frames: AsyncIterable<RuntimeWatchView>;
      close: Effect.Effect<void>;
    }
  | {
      status: "no_connection";
    }
  | {
      status: "unreachable";
      error: PloyzProviderError | null;
    };

/** A feature's view of one Organization's frames: passes them on and may act on what it sees. */
export type RuntimeWatchTap = (organizationId: string) => (frames: AsyncIterable<RuntimeWatchView>) => AsyncIterable<RuntimeWatchView>;

export const openRuntimeWatch = Effect.fn("Runtime.openWatch")(function* (input: {
  request: Request;
  organizationSlug: string;
  tap?: RuntimeWatchTap | undefined;
}) {
  const access = yield* authorizeRequest(input.request.headers, input.organizationSlug, "project:read");
  return yield* openRuntimeWatchForOrganization({
    request: input.request,
    organizationId: access.organization.id,
    tap: input.tap,
  });
});

export const openRuntimeWatchForOrganization = Effect.fn(
  "Runtime.openWatchForOrganization",
)(function* (input: { request: Request; organizationId: string; tap?: RuntimeWatchTap | undefined }) {
  if (import.meta.env.DEV && (yield* AppConfig).verify.runtime) {
    const { verifyRuntimeFrameHeldOpen } = yield* Effect.promise(() => import("./runtime-watch.verify-fixture"));
    return { status: "connected" as const, frames: verifyRuntimeFrameHeldOpen(input.request.signal), close: Effect.void } satisfies OpenedRuntimeWatch;
  }
  const scope = yield* Scope.make();
  const close = Scope.close(scope, Exit.void);
  const runtime = yield* OrganizationRuntime;
  const session = yield* runtime.open(input.organizationId).pipe(
    Effect.provideService(Scope.Scope, scope),
    Effect.onError(() => close),
  );
  if (session.status !== "connected") {
    yield* close;
    return session;
  }

  const frames = yield* session.connected
    .watch({ signal: input.request.signal })
    .pipe(Effect.onError(() => close));
  return {
    status: "connected" as const,
    frames: input.tap === undefined ? frames : input.tap(input.organizationId)(frames),
    close,
  } satisfies OpenedRuntimeWatch;
});
