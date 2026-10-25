import "@tanstack/react-start/server-only";
import type { RuntimeWatchView } from "@ployz/sdk";
import { Effect, Exit, Logger, Scope } from "effect";
import { redactedLoggers } from "#/server/log-redaction";
import { onIngressChange } from "#/modules/cluster-domain/ingress-running";
import { InngestClient, sendInngestEvent } from "#/modules/inngest/client";
import {
  createClusterDomainSyncRequestedEvent,
  type InngestSendableEvent,
} from "#/modules/inngest/events";
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
  const access = yield* authorizeRequest(input.request.headers, input.organizationSlug, "project:read");
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
  const sendOnChange = (event: InngestSendableEvent, failed: string) => () => Effect.runFork(
    sendInngestEvent(event).pipe(
      Effect.provideService(InngestClient, inngest),
      Effect.catch((error) => Effect.logWarning(failed, error)),
      Effect.provideService(Logger.CurrentLoggers, redactedLoggers),
    ),
  );
  const { organizationId } = input;
  const requestDomainSync = sendOnChange(
    createClusterDomainSyncRequestedEvent({ organizationId }),
    "Cluster Domain sync request failed; the hourly sync covers it.",
  );
  return {
    status: "connected" as const,
    frames: onIngressChange(frames, requestDomainSync),
    close,
  } satisfies OpenedRuntimeWatch;
});
