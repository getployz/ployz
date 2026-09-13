import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, useHydrated } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { toast } from "sonner";
import { DashboardPage } from "#/components/dashboard-page";
import { RouteErrorAlert } from "#/components/route-error-alert";
import {
  Card,
  CardContent,
  CardFooter,
  CardHeader,
} from "#/components/ui/card";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "#/components/ui/empty";
import { Skeleton } from "#/components/ui/skeleton";
import { authClient } from "#/auth/auth-client";
import {
  BillingPlanChangeDialog,
  type BillingPlanChangePreview,
} from "#/routes/_protected/cloud/$organizationSlug/_org/-components/BillingPlanChangeDialog";
import { BillingPlanCard } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/BillingPlanCard";
import {
  billingPlans,
  type BillingPlanSlug,
} from "#/routes/_protected/cloud/$organizationSlug/_org/-components/billing-plans";
import {
  billingKeys,
  billingStateQueryOptions,
} from "#/modules/billing/billing.queries";
import {
  createEmbeddedCheckoutServerFn,
  previewSubscriptionPlanChangeServerFn,
  updateSubscriptionPlanServerFn,
} from "#/modules/billing/billing.functions";
import { organizationStateQueryOptions } from "#/modules/environment-design/workspace-queries";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/billing"
)({
  loader: ({ params, context }) =>
    Promise.all([
      context.queryClient.ensureQueryData(
        billingStateQueryOptions(params.organizationSlug)
      ),
      context.queryClient.ensureQueryData(
        organizationStateQueryOptions(params.organizationSlug)
      ),
    ]),
  pendingComponent: BillingPending,
  errorComponent: BillingError,
  component: RouteComponent,
});

function BillingPending() {
  return (
    <DashboardPage density="spacious" width="wide">
      <div className="flex max-w-2xl flex-col gap-3">
        <Skeleton className="h-10 w-64 max-w-full" />
        <Skeleton className="h-6 w-full" />
        <Skeleton className="h-6 w-4/5" />
      </div>
      <section className="grid gap-6 xl:grid-cols-3">
        {Array.from({ length: 3 }, (_, index) => (
          <Card key={index} className="min-h-96">
            <CardHeader className="flex flex-col gap-3">
              <Skeleton className="h-5 w-24" />
              <Skeleton className="h-5 w-32" />
              <Skeleton className="h-4 w-full" />
              <Skeleton className="h-10 w-28" />
            </CardHeader>
            <CardContent className="flex flex-1 flex-col gap-3">
              {Array.from({ length: 4 }, (_, featureIndex) => (
                <Skeleton key={featureIndex} className="h-4 w-full" />
              ))}
            </CardContent>
            <CardFooter>
              <Skeleton className="h-9 w-full" />
            </CardFooter>
          </Card>
        ))}
      </section>
    </DashboardPage>
  );
}

function BillingError() {
  return (
    <DashboardPage density="spacious" width="wide">
      <RouteErrorAlert
        title="Billing couldn’t load"
        description="Plan and subscription details are unavailable right now. Try loading them again."
      />
    </DashboardPage>
  );
}

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const isHydrated = useHydrated();
  const queryClient = useQueryClient();
  const { data: billingState } = useSuspenseQuery(
    billingStateQueryOptions(organizationSlug)
  );
  const [pendingPlan, setPendingPlan] = useState<string | null>(null);
  const [changeDialogOpen, setChangeDialogOpen] = useState(false);
  const [changePreview, setChangePreview] =
    useState<BillingPlanChangePreview | null>(null);
  const createEmbeddedCheckout = useServerFn(createEmbeddedCheckoutServerFn);
  const previewSubscriptionPlanChange = useServerFn(
    previewSubscriptionPlanChangeServerFn
  );
  const updateSubscriptionPlan = useServerFn(updateSubscriptionPlanServerFn);
  const activeCheckoutRef = useRef<{ close(): void } | null>(null);
  const previewCurrencyFormatter = useMemo(
    () =>
      new Intl.NumberFormat("en-US", {
        style: "currency",
        currency: changePreview?.currency.toUpperCase() ?? "USD",
      }),
    [changePreview?.currency]
  );

  function formatPreviewCurrency(amount: number) {
    return previewCurrencyFormatter.format(amount / 100);
  }

  const closeActiveCheckout = useCallback(() => {
    const activeCheckout = activeCheckoutRef.current;
    activeCheckoutRef.current = null;
    activeCheckout?.close();
  }, []);

  useEffect(() => {
    return closeActiveCheckout;
  }, [closeActiveCheckout]);

  if (billingState.billingMode === "self_hosted") {
    return (
      <DashboardPage density="spacious" width="wide">
        <Empty variant="first-run">
          <EmptyHeader>
            <EmptyTitle>
              Billing is managed by this installation
            </EmptyTitle>
            <EmptyDescription>
              Custom domains and other capabilities are available without a
              hosted Ployz subscription.
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      </DashboardPage>
    );
  }

  async function openBillingPortal(planSlug: BillingPlanSlug) {
    try {
      setPendingPlan(planSlug);
      const response = await authClient.customer.portal();
      const portalUrl = response.data?.url;

      if (!portalUrl) {
        throw new Error("Missing portal URL");
      }

      window.location.href = portalUrl;
    } catch {
      toast.error("Unable to open billing portal.");
    } finally {
      setPendingPlan(null);
    }
  }

  async function openPlanChangeDialog(planSlug: BillingPlanSlug) {
    try {
      setPendingPlan(planSlug);
      const preview = await previewSubscriptionPlanChange({
        data: {
          organizationSlug,
          plan: planSlug,
        },
      });

      setChangePreview(preview);
      setChangeDialogOpen(true);
    } catch {
      toast.error("Unable to preview plan change.");
    } finally {
      setPendingPlan(null);
    }
  }

  async function confirmPlanChange() {
    if (!changePreview) {
      return;
    }

    try {
      setPendingPlan(changePreview.targetPlan);
      await updateSubscriptionPlan({
        data: {
          organizationSlug,
          plan: changePreview.targetPlan,
        },
      });
      await queryClient.invalidateQueries({
        queryKey: billingKeys.state(organizationSlug),
      });
      setChangeDialogOpen(false);
      setChangePreview(null);
    } catch {
      toast.error("Unable to change plan.");
    } finally {
      setPendingPlan(null);
    }
  }

  async function handleCheckout(planSlug: BillingPlanSlug) {
    const isCurrentPlan = billingState.currentPlan === planSlug;

    if (billingState.hasActiveSubscription && isCurrentPlan) {
      await openBillingPortal(planSlug);
      return;
    }

    if (billingState.hasActivePaidSubscription) {
      await openPlanChangeDialog(planSlug);
      return;
    }

    try {
      setPendingPlan(planSlug);
      const checkout = await createEmbeddedCheckout({
        data: {
          organizationSlug,
          plan: planSlug,
        },
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
      setPendingPlan(null);
    }
  }

  return (
    <>
      <DashboardPage density="spacious" width="wide">
        <div className="flex max-w-2xl flex-col gap-3">
          <h1 className="text-4xl font-semibold tracking-tight text-balance">
            Choose a plan
          </h1>
          <p className="text-lg text-muted-foreground text-balance">
            Every plan includes unlimited servers. Upgrade for retention,
            backups, and production features.
          </p>
        </div>

        <section className="grid gap-6 xl:grid-cols-3">
          {billingPlans.map((plan) => {
            const isCurrentPlan = billingState.currentPlan === plan.slug;
            const ctaLabel = billingState.hasActiveSubscription
              ? isCurrentPlan
                ? "Manage billing"
                : "Change plan"
              : isCurrentPlan
              ? "Current plan"
              : plan.ctaLabel;

            return (
              <BillingPlanCard
                key={plan.name}
                plan={plan}
                ctaLabel={ctaLabel}
                disabled={
                  !isHydrated ||
                  pendingPlan !== null ||
                  (!billingState.hasActiveSubscription && isCurrentPlan)
                }
                isCurrentPlan={isCurrentPlan}
                isPending={pendingPlan === plan.slug}
                onCheckout={() => handleCheckout(plan.slug)}
              />
            );
          })}
        </section>
      </DashboardPage>

      <BillingPlanChangeDialog
        open={changeDialogOpen}
        pendingPlan={pendingPlan}
        preview={changePreview}
        formatCurrency={formatPreviewCurrency}
        onConfirm={() => void confirmPlanChange()}
        onOpenChange={(open) => {
          setChangeDialogOpen(open);
          if (!open) {
            setChangePreview(null);
          }
        }}
      />
    </>
  );
}
