import { useState } from "react";
import { useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, useHydrated } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { CheckIcon } from "lucide-react";
import { toast } from "sonner";
import { DashboardPage } from "#/components/dashboard-page";
import { RouteErrorAlert } from "#/components/route-error-alert";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Item, ItemActions, ItemContent, ItemDescription, ItemTitle } from "#/components/ui/item";
import { Skeleton } from "#/components/ui/skeleton";
import { Spinner } from "#/components/ui/spinner";
import { cn } from "#/lib/utils";
import { billingStateQueryOptions } from "#/modules/billing/billing.queries";
import { createCustomerPortalServerFn } from "#/modules/billing/billing.functions";
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
    <DashboardPage width="content">
      <Skeleton className="h-5 w-72" />
      <div className="grid overflow-hidden rounded-xl border md:grid-cols-3">
        {[0, 1, 2].map((column) => (
          <div key={column} className="flex flex-col gap-5 p-6">
            <Skeleton className="h-5 w-16" />
            <Skeleton className="h-10 w-24" />
            <Skeleton className="h-20 w-full" />
          </div>
        ))}
      </div>
    </DashboardPage>
  );
}

function BillingError() {
  return (
    <DashboardPage width="content">
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
  const createCustomerPortal = useServerFn(createCustomerPortalServerFn);
  const { data: billingState } = useSuspenseQuery(
    billingStateQueryOptions(organizationSlug)
  );
  const [pending, setPending] = useState(false);

  /** Cancellation and payment changes happen in the Polar portal. */
  async function openBillingPortal() {
    try {
      setPending(true);
      const { customerPortalUrl } = await createCustomerPortal({ data: { organizationSlug } });
      window.location.href = customerPortalUrl;
    } catch {
      toast.error("Unable to open billing portal.");
    } finally {
      setPending(false);
    }
  }

  return (
    <DashboardPage width="content">
      <p className="text-muted-foreground">
        Ployz is free on your own servers, custom domains included. Paid plans are coming soon.
      </p>
      <div className="grid overflow-hidden rounded-xl border md:grid-cols-3">
        {PLANS.map((plan, index) => (
          <Plan
            key={plan.name}
            plan={plan}
            current={index === 0}
            className={index === 0 ? undefined : "border-t md:border-t-0 md:border-l"}
          />
        ))}
      </div>
      {billingState.hasActiveSubscription ? (
        <Item variant="outline">
          <ItemContent>
            <ItemTitle>{periodEndCopy(billingState)}</ItemTitle>
            <ItemDescription>
              Your Pro subscription paid for custom domains, which are free now. Cancel it in the billing portal.
            </ItemDescription>
          </ItemContent>
          <ItemActions>
            <Button variant="outline" disabled={!isHydrated || pending} onClick={() => void openBillingPortal()}>
              {pending ? <Spinner data-icon="inline-start" /> : null}
              Manage billing
            </Button>
          </ItemActions>
        </Item>
      ) : null}
    </DashboardPage>
  );
}

type PlanDetails = {
  readonly name: string;
  readonly price: string;
  readonly per: string;
  readonly pitch: string;
  readonly features: readonly string[];
  readonly soon?: true;
};

const PLANS: readonly PlanDetails[] = [
  {
    name: "Free",
    price: "$0",
    per: "/ month",
    pitch: "The whole platform, on servers you own. No card.",
    features: [
      "Unlimited servers and projects",
      "Dashboard and git push deploys",
      "Preview environments",
      "One-click databases",
      "Custom domains, and a free *.ployz.app address",
      "Live logs and server metrics",
      "24 hours of logs and metrics",
    ],
  },
  {
    name: "Hobby",
    price: "$5",
    per: "/ month",
    pitch: "For the app that pays your rent.",
    features: [
      "Everything in Free",
      "7 days of logs and metrics",
      "Uptime checks from outside your servers",
      "Alerts by email and Slack",
    ],
    soon: true,
  },
  {
    name: "Pro",
    price: "$49",
    per: "/ month per organization",
    pitch: "For a team. Not per seat, not per server.",
    features: [
      "Everything in Hobby",
      "30 days of logs and metrics",
      "Unlimited members",
      "Roles and audit log",
      "Public status page",
    ],
    soon: true,
  },
];

function Plan({ plan, current, className }: { plan: PlanDetails; current: boolean; className?: string }) {
  return (
    <section
      aria-label={plan.name}
      className={cn("flex flex-col gap-5 p-6", current && "bg-muted/50", className)}
    >
      <div className="flex items-center justify-between">
        <h2 className="font-medium">{plan.name}</h2>
        {current ? <Badge variant="outline">Current</Badge> : null}
        {plan.soon ? <Badge variant="secondary">Coming soon</Badge> : null}
      </div>
      <div className="flex flex-col gap-1">
        <p className="text-4xl font-semibold tracking-tight">
          {plan.price}
          <span className="text-muted-foreground text-sm font-normal"> {plan.per}</span>
        </p>
        <p className="text-muted-foreground text-sm">{plan.pitch}</p>
      </div>
      <ul className="flex flex-col gap-2.5 text-sm">
        {plan.features.map((feature) => (
          <li key={feature} className="flex items-center gap-2.5">
            <CheckIcon aria-hidden className={cn("size-4 shrink-0", plan.soon ? "text-muted-foreground" : "text-primary")} />
            {feature}
          </li>
        ))}
      </ul>
    </section>
  );
}

// UTC so the server render and the browser agree on the day.
const periodEndFormat = new Intl.DateTimeFormat("en-US", { dateStyle: "medium", timeZone: "UTC" });

function periodEndCopy(state: { currentPeriodEnd: Date | null; cancelAtPeriodEnd: boolean }) {
  if (state.currentPeriodEnd === null) return "Your Pro subscription is active";
  const day = periodEndFormat.format(state.currentPeriodEnd);
  return state.cancelAtPeriodEnd ? `Your Pro subscription ends on ${day}` : `Your Pro subscription renews on ${day}`;
}
