import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createOrganizationBillingSyncRequestedEvent } from "#/modules/inngest/events";

/**
 * Asks the billing sync to bring the subscription's units in line with the Server count. A send that fails only waits for
 * the nightly reconcile. A Self-hosted Cloud registers no billing sync, so the event goes unread there.
 */
export const requestBillingSync = Effect.fn("Billing.requestSync")(function* (
  organizationId: string,
  reason: "server-joined" | "server-removed",
) {
  yield* sendInngestEvent(createOrganizationBillingSyncRequestedEvent({ organizationId, reason })).pipe(
    // Callers include the runtime recorder's uninterruptible writer; a stalled send must not hold it.
    Effect.timeoutOrElse({
      duration: "10 seconds",
      orElse: () => Effect.fail(new Error("Requesting the billing sync timed out")),
    }),
    Effect.interruptible,
    Effect.catch((error) => Effect.logWarning("Billing units wait for the nightly reconcile.", { organizationId, reason, error })),
  );
});
