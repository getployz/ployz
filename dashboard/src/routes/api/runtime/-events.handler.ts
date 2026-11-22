import { Effect } from "effect";
import { createRuntimeEventsResponse } from "#/modules/runtime/runtime-events.server";
import { openRuntimeWatch } from "#/modules/runtime/runtime-watch.server";

/** The stream of one Organization's runtime watch. Closing the watch outlives this Effect, so it keeps this one's services. */
export const handleRuntimeEventsRequest = Effect.fn("Runtime.events")(function* (request: Request, organizationSlug: string) {
  const watch = yield* openRuntimeWatch({ request, organizationSlug });
  if (watch.status !== "connected") return createRuntimeEventsResponse({ request, ...watch });
  const services = yield* Effect.context<never>();
  return createRuntimeEventsResponse({
    request,
    status: "connected",
    frames: watch.frames,
    close: () => Effect.runPromiseWith(services)(watch.close),
  });
});
