import "@tanstack/react-start/server-only";
import type { RuntimeWatchView } from "@ployz/sdk";
import { Effect, Exit, Scope } from "effect";
import { authorizeRuntimeOrganization } from "#/modules/runtime/authorize-runtime-organization.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzProviderError } from "#/modules/runtime/ployz.server";

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

export const openRuntimeWatch = Effect.fn("Runtime.openWatch")(function* (input: {
  request: Request;
  organizationSlug: string;
}) {
  const authorized = yield* authorizeRuntimeOrganization({
    headers: input.request.headers,
    organizationSlug: input.organizationSlug,
  });
  return yield* openRuntimeWatchForOrganization({
    request: input.request,
    organizationId: authorized.organizationId,
  });
});

export const openRuntimeWatchForOrganization = Effect.fn(
  "Runtime.openWatchForOrganization",
)(function* (input: { request: Request; organizationId: string }) {
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
    frames,
    close,
  } satisfies OpenedRuntimeWatch;
});
