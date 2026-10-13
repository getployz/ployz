import "@tanstack/react-start/server-only";
import type { RuntimeWatchView } from "@ployz/sdk";
import { Effect, Exit, Scope } from "effect";
import { onIngressChange } from "#/modules/cluster-domain/ingress-running";
import { InngestClient, sendInngestEvent } from "#/modules/inngest/client";
import { createClusterDomainSyncRequestedEvent } from "#/modules/inngest/events";
import { authorizeRequest } from "#/modules/organization/access.server";
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
  const access = yield* authorizeRequest(input.request.headers, input.organizationSlug, "operate");
  return yield* openRuntimeWatchForOrganization({
    request: input.request,
    organizationId: access.organization.id,
  });
});

export const openRuntimeWatchForOrganization = Effect.fn(
  "Runtime.openWatchForOrganization",
)(function* (input: { request: Request; organizationId: string }) {
  if (import.meta.env.DEV && process.env["PLOYZ_VERIFY_RUNTIME"] === "1") {
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
  const inngest = yield* InngestClient;
  const requestDomainSync = () => Effect.runFork(
    sendInngestEvent(createClusterDomainSyncRequestedEvent({ organizationId: input.organizationId })).pipe(
      Effect.provideService(InngestClient, inngest),
      Effect.catch((error) => Effect.logWarning("Cluster Domain sync request failed; the hourly sync covers it.", error)),
    ),
  );
  return {
    status: "connected" as const,
    frames: onIngressChange(frames, requestDomainSync),
    close,
  } satisfies OpenedRuntimeWatch;
});
