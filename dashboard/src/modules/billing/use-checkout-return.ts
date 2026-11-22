import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useServerFn } from "@tanstack/react-start";
import { toast } from "sonner";
import { syncBillingAfterCheckoutServerFn } from "#/modules/billing/billing.functions";
import { billingKeys } from "#/modules/billing/billing.queries";
import { uptimeKeys } from "#/modules/uptime/uptime.queries";
import { analyticsKeys } from "#/modules/web-analytics/ui/analytics.queries";

/** On the redirect back from checkout, `clear` drops `checkout_id` from the address and the subscription is read again. */
export function useSyncBillingAfterCheckout(organizationSlug: string, checkoutId: string | undefined, clear: () => void) {
  const queryClient = useQueryClient();
  const syncBillingAfterCheckout = useServerFn(syncBillingAfterCheckoutServerFn);
  useEffect(() => {
    if (checkoutId === undefined) return;
    clear();
    // The webhook may land a moment after the redirect, so the page reads the subscription itself.
    void syncBillingAfterCheckout({ data: { organizationSlug } })
      .then((active) => {
        if (active) toast.success("Your cluster is in production. Thanks for supporting Ployz!");
      })
      // A failed sync only skips the toast: the refetches below read the subscription the webhook lands.
      .catch(() => undefined)
      .finally(() => {
        void queryClient.invalidateQueries({ queryKey: billingKeys.org(organizationSlug) });
        // Production opens uptime and analytics, which may hold the answer that asked for it.
        for (const queryKey of [uptimeKeys.all, analyticsKeys.all, analyticsKeys.requests]) void queryClient.invalidateQueries({ queryKey });
      });
  }, [checkoutId]);
}
