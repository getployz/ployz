import { useCallback, useEffect, useRef, useState } from "react";
import { useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, useHydrated } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { toast } from "sonner";
import { DashboardPage } from "#/components/dashboard-page";
import { RouteErrorAlert } from "#/components/route-error-alert";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import {
  Card,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "#/components/ui/card";
import { Skeleton } from "#/components/ui/skeleton";
import { Spinner } from "#/components/ui/spinner";
import { authClient } from "#/auth/auth-client";
import { billingStateQueryOptions } from "#/modules/billing/billing.queries";
import { createEmbeddedCheckoutServerFn } from "#/modules/billing/billing.functions";
import { prefetchRemote, requireBilling } from "#/collections/route-data";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/billing"
)({
  loader: async ({ params, context }) => {
    await requireBilling(context, params.organizationSlug);
    await prefetchRemote(context, billingStateQueryOptions(params.organizationSlug));
  },
  pendingComponent: BillingPending,
  errorComponent: BillingError,
  component: RouteComponent,
});

function BillingPending() {
  return (
    <DashboardPage width="wide">
      <Card className="max-w-md">
        <CardHeader className="flex flex-col gap-3">
          <Skeleton className="h-5 w-24" />
          <Skeleton className="h-4 w-full" />
        </CardHeader>
        <CardFooter>
          <Skeleton className="h-9 w-full" />
        </CardFooter>
      </Card>
    </DashboardPage>
  );
}

function BillingError() {
  return (
    <DashboardPage width="wide">
      <RouteErrorAlert
        title="Billing couldn’t load"
        description="Subscription details are unavailable right now. Try loading them again."
      />
    </DashboardPage>
  );
}

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const isHydrated = useHydrated();
  const { data: billingState } = useSuspenseQuery(
    billingStateQueryOptions(organizationSlug)
  );
  const [pending, setPending] = useState(false);
  const createEmbeddedCheckout = useServerFn(createEmbeddedCheckoutServerFn);
  const activeCheckoutRef = useRef<{ close(): void } | null>(null);

  const closeActiveCheckout = useCallback(() => {
    const activeCheckout = activeCheckoutRef.current;
    activeCheckoutRef.current = null;
    activeCheckout?.close();
  }, []);

  useEffect(() => {
    return closeActiveCheckout;
  }, [closeActiveCheckout]);

  /** Cancellation and payment changes happen in the Polar portal. */
  async function openBillingPortal() {
    try {
      setPending(true);
      const response = await authClient.customer.portal();
      const portalUrl = response.data?.url;

      if (!portalUrl) {
        throw new Error("Missing portal URL");
      }

      window.location.href = portalUrl;
    } catch {
      toast.error("Unable to open billing portal.");
    } finally {
      setPending(false);
    }
  }

  async function openCheckout() {
    try {
      setPending(true);
      const checkout = await createEmbeddedCheckout({
        data: { organizationSlug },
      });

      closeActiveCheckout();

      const { PolarEmbedCheckout } = await import("@polar-sh/checkout/embed");
      const activeCheckout = await PolarEmbedCheckout.create(checkout.url, {
        theme: "light",
      });

      activeCheckout.addEventListener("close", () => {
        if (activeCheckoutRef.current === activeCheckout) {
          activeCheckoutRef.current = null;
        }
      });
      activeCheckoutRef.current = activeCheckout;
    } catch {
      toast.error("Unable to start checkout.");
    } finally {
      setPending(false);
    }
  }

  const subscribed = billingState.hasActiveSubscription;
  const action = subscribed
    ? { label: "Manage billing", variant: "outline", run: openBillingPortal } as const
    : { label: "Get Started", variant: "default", run: openCheckout } as const;

  return (
    <DashboardPage width="wide">
      <Card className="max-w-md">
        <CardHeader className="flex flex-col gap-3">
          {subscribed ? <Badge variant="secondary">Current</Badge> : null}
          {/* Copy decided in #1007; it describes the POLAR_PRODUCT_ID product. */}
          <CardTitle>Pro</CardTitle>
          <CardDescription>$9/mo · custom domains on Services</CardDescription>
        </CardHeader>
        <CardFooter>
          <Button
            className="w-full"
            variant={action.variant}
            disabled={!isHydrated || pending}
            onClick={() => void action.run()}
          >
            {pending ? <Spinner data-icon="inline-start" /> : null}
            {action.label}
          </Button>
        </CardFooter>
      </Card>
    </DashboardPage>
  );
}
