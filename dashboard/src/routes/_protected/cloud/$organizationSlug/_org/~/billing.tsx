import { useEffect, useState } from "react";
import { useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, useHydrated, useNavigate } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
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
import { MINIMUM_UNITS, monthlyPriceUsd, UNIT_PRICE_USD } from "#/modules/billing/billing";
import { billingKeys, billingStateQueryOptions } from "#/modules/billing/billing.queries";
import {
  createCustomerPortalServerFn,
  syncBillingAfterCheckoutServerFn,
} from "#/modules/billing/billing.functions";
import { useEmbeddedCheckout } from "#/modules/billing/use-embedded-checkout";
import { prefetchRemote, requireBilling } from "#/collections/route-data";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/billing"
)({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ checkout_id: Schema.optional(Schema.String) })),
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
    <DashboardPage width="content" title="Organization">
      <Skeleton className="h-5 w-72" />
      <div className="grid overflow-hidden rounded-xl border md:grid-cols-2">
        {[0, 1].map((column) => (
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
    <DashboardPage width="content" title="Organization">
      <RouteErrorAlert
        title="Billing couldn’t load"
        description="Subscription details are unavailable right now. Try loading them again."
      />
    </DashboardPage>
  );
}

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { checkout_id: checkoutId } = Route.useSearch();
  const navigate = useNavigate({ from: Route.fullPath });
  const queryClient = useQueryClient();
  const isHydrated = useHydrated();
  const createCustomerPortal = useServerFn(createCustomerPortalServerFn);
  const syncBillingAfterCheckout = useServerFn(syncBillingAfterCheckoutServerFn);
  const { data: billingState } = useSuspenseQuery(
    billingStateQueryOptions(organizationSlug)
  );
  const [portalPending, setPortalPending] = useState(false);
  const { openCheckout, pending: checkoutPending } = useEmbeddedCheckout(organizationSlug);
  const pending = portalPending || checkoutPending;

  useEffect(() => {
    if (checkoutId === undefined) return;
    void navigate({ search: {}, replace: true });
    // The webhook may land a moment after the redirect, so the page reads the subscription itself.
    void syncBillingAfterCheckout({ data: { organizationSlug } })
      .then((active) => {
        if (active) toast.success("Your cluster is in production. Thanks for supporting Ployz!");
      })
      .catch(() => undefined)
      .finally(() => {
        void queryClient.invalidateQueries({ queryKey: billingKeys.org(organizationSlug) });
        // Production opens uptime and analytics, which may hold the answer that asked for it.
        for (const key of ["uptime", "analytics", "analytics-requests"]) void queryClient.invalidateQueries({ queryKey: [key] });
      });
  }, [checkoutId]);

  /** Cancellation and payment changes happen in the Polar portal. */
  async function openBillingPortal() {
    try {
      setPortalPending(true);
      const { customerPortalUrl } = await createCustomerPortal({ data: { organizationSlug } });
      window.location.href = customerPortalUrl;
    } catch {
      toast.error("Unable to open billing portal.");
    } finally {
      setPortalPending(false);
    }
  }

  const subscribed = billingState.hasActiveSubscription;
  const busy = !isHydrated || pending;

  return (
    <DashboardPage width="content" title="Organization">
      <p className="text-muted-foreground">
        {subscribed
          ? "Your cluster is in production."
          : "Free on your own servers. Pay only when your cluster is in production."}
      </p>
      <div className="grid overflow-hidden rounded-xl border md:grid-cols-2">
        <Plan
          name="Free"
          price="$0"
          pitch="The whole platform, on servers you own."
          current={!subscribed}
          features={FREE_FEATURES}
        />
        <Plan
          name="Production"
          price={`$${MINIMUM_UNITS * UNIT_PRICE_USD}`}
          pitch={`Includes ${MINIMUM_UNITS} servers, then $${UNIT_PRICE_USD} per extra server.`}
          current={subscribed}
          features={PRODUCTION_FEATURES}
          className="border-t md:border-t-0 md:border-l"
        >
          {subscribed || !billingState.canBuy ? null : (
            <Button className="self-start" disabled={busy} onClick={() => void openCheckout()}>
              {checkoutPending ? <Spinner data-icon="inline-start" /> : null}
              Upgrade to production
            </Button>
          )}
        </Plan>
      </div>
      {subscribed ? (
        <Item variant="outline">
          <ItemContent>
            <ItemTitle>{periodEndCopy(billingState)}</ItemTitle>
            <ItemDescription>
              {serversCopy(billingState.servers)} · ${monthlyPriceUsd(billingState.servers)} a month
            </ItemDescription>
          </ItemContent>
          <ItemActions>
            <Button variant="outline" disabled={busy} onClick={() => void openBillingPortal()}>
              {portalPending ? <Spinner data-icon="inline-start" /> : null}
              Manage billing
            </Button>
          </ItemActions>
        </Item>
      ) : null}
    </DashboardPage>
  );
}

type Feature = { readonly label: string; readonly soon?: true };

const FREE_FEATURES: readonly Feature[] = [
  { label: "Unlimited servers and projects" },
  { label: "Dashboard and git push deploys" },
  { label: "Preview environments" },
  { label: "One-click databases" },
  { label: "Custom domains, and a free *.ployz.app address" },
  { label: "Live logs and server metrics" },
  { label: "24 hours of logs and metrics" },
];

const PRODUCTION_FEATURES: readonly Feature[] = [
  { label: "Everything in Free" },
  { label: "Uptime checks from outside your servers" },
  { label: "Uptime alerts by email" },
  { label: "Web analytics for your domains" },
  { label: "Longer log and metrics history", soon: true },
  { label: "Slack alerts", soon: true },
];

function Plan({
  name,
  price,
  pitch,
  current,
  features,
  className,
  children,
}: {
  name: string;
  price: string;
  pitch: string;
  current: boolean;
  features: readonly Feature[];
  className?: string;
  children?: React.ReactNode;
}) {
  return (
    <section
      aria-label={name}
      className={cn("flex flex-col gap-5 p-6", current && "bg-muted/50", className)}
    >
      <div className="flex items-center justify-between">
        <h2 className="font-medium">{name}</h2>
        {current ? <Badge variant="outline">Current</Badge> : null}
      </div>
      <div className="flex flex-col gap-1">
        <p className="text-2xl font-medium">
          {price}
          <span className="text-muted-foreground text-sm font-normal"> / month</span>
        </p>
        <p className="text-muted-foreground text-sm">{pitch}</p>
      </div>
      <ul className="flex flex-col gap-2.5 text-sm">
        {features.map((feature) => (
          <li
            key={feature.label}
            className={cn("flex items-center gap-2.5", feature.soon && "text-muted-foreground")}
          >
            <CheckIcon aria-hidden className={cn("size-4 shrink-0", feature.soon ? "opacity-40" : "text-primary")} />
            {feature.label}
            {feature.soon ? <Badge variant="outline">Soon</Badge> : null}
          </li>
        ))}
      </ul>
      {children}
    </section>
  );
}

const serversCopy = (servers: number) => (servers === 1 ? "1 server" : `${servers} servers`);

// UTC so the server render and the browser agree on the day.
const periodEndFormat = new Intl.DateTimeFormat("en-US", { dateStyle: "medium", timeZone: "UTC" });

function periodEndCopy(state: { currentPeriodEnd: Date | null; cancelAtPeriodEnd: boolean }) {
  if (state.currentPeriodEnd === null) return "Production is active";
  const day = periodEndFormat.format(state.currentPeriodEnd);
  return state.cancelAtPeriodEnd ? `Production ends on ${day}` : `Renews on ${day}`;
}
