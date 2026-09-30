import { useCallback, useEffect, useRef, useState } from "react";
import { useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, useHydrated, useNavigate } from "@tanstack/react-router";
import { Schema } from "effect";
import { useServerFn } from "@tanstack/react-start";
import { CheckIcon, HeartIcon } from "lucide-react";
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
import { createCustomerPortalServerFn, createEmbeddedCheckoutServerFn } from "#/modules/billing/billing.functions";
import { openCheckoutWhileHere } from "#/modules/billing/checkout";
import { prefetchRemote, requireBilling } from "#/collections/route-data";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/billing"
)({
  // Polar's checkout comes back here with the checkout it completed.
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
    <DashboardPage width="content">
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
  const { checkout_id: checkoutId } = Route.useSearch();
  const navigate = useNavigate({ from: Route.fullPath });
  const queryClient = useQueryClient();
  const isHydrated = useHydrated();
  const createCustomerPortal = useServerFn(createCustomerPortalServerFn);

  useEffect(() => {
    if (checkoutId === undefined) return;
    toast.success("You're on Pro. Thanks for supporting Ployz!");
    void queryClient.invalidateQueries(billingStateQueryOptions(organizationSlug));
    void navigate({ search: {}, replace: true });
  }, [checkoutId]);
  const { data: billingState } = useSuspenseQuery(
    billingStateQueryOptions(organizationSlug)
  );
  const [pending, setPending] = useState(false);
  const createEmbeddedCheckout = useServerFn(createEmbeddedCheckoutServerFn);
  const activeCheckoutRef = useRef<{ close(): void } | null>(null);
  // Identifies this organization's visit; a checkout that resolves after it ends must not open.
  const visitRef = useRef<object | null>(null);

  const closeActiveCheckout = useCallback(() => {
    const activeCheckout = activeCheckoutRef.current;
    activeCheckoutRef.current = null;
    activeCheckout?.close();
  }, []);

  useEffect(() => {
    visitRef.current = {};
    return () => {
      visitRef.current = null;
      closeActiveCheckout();
    };
  }, [organizationSlug, closeActiveCheckout]);

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

  async function openCheckout() {
    const visit = visitRef.current;
    try {
      setPending(true);
      const activeCheckout = await openCheckoutWhileHere({
        createUrl: async () => (await createEmbeddedCheckout({ data: { organizationSlug } })).url,
        open: async (url) => {
          closeActiveCheckout();
          const { PolarEmbedCheckout } = await import("@polar-sh/checkout/embed");
          return PolarEmbedCheckout.create(url, { theme: "light" });
        },
        left: () => visitRef.current !== visit,
      });
      if (!activeCheckout) return;

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
  const busy = !isHydrated || pending;

  return (
    <DashboardPage width="content">
      <p className="text-muted-foreground">
        {subscribed
          ? "You’re on Pro. Thanks for supporting Ployz."
          : "Free on your own servers. Pro adds custom domains."}
      </p>
      <div className="grid overflow-hidden rounded-xl border md:grid-cols-2">
        <Plan name="Free" price="$0" current={!subscribed} features={FREE_FEATURES} />
        <Plan
          name="Pro"
          price="$9"
          current={subscribed}
          features={PRO_FEATURES}
          className="border-t md:border-t-0 md:border-l"
        >
          {subscribed ? null : (
            <Button className="self-start" disabled={busy} onClick={() => void openCheckout()}>
              {pending ? <Spinner data-icon="inline-start" /> : null}
              Upgrade to Pro · $9/mo
            </Button>
          )}
        </Plan>
      </div>
      {subscribed ? (
        <Item variant="outline">
          <ItemContent>
            <ItemTitle>{periodEndCopy(billingState)}</ItemTitle>
            <ItemDescription>Payment method and invoices</ItemDescription>
          </ItemContent>
          <ItemActions>
            <Button variant="outline" disabled={busy} onClick={() => void openBillingPortal()}>
              {pending ? <Spinner data-icon="inline-start" /> : null}
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
  { label: "Preview environments" },
  { label: "Open source" },
];

// Custom domains are Pro's only feature today; the rest is what paying supports.
const PRO_FEATURES: readonly Feature[] = [
  { label: "Custom domains on any service" },
  { label: "Metrics", soon: true },
  { label: "Log retention", soon: true },
  { label: "Cloud builds", soon: true },
];

function Plan({
  name,
  price,
  current,
  features,
  className,
  children,
}: {
  name: string;
  price: string;
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
      <p className="text-4xl font-semibold tracking-tight">
        {price}
        <span className="text-muted-foreground text-sm font-normal"> /mo</span>
      </p>
      <ul className="flex flex-col gap-2.5 text-sm">
        {features.map((feature) => (
          <li
            key={feature.label}
            className={cn("flex items-center gap-2.5", feature.soon && "text-muted-foreground")}
          >
            <CheckIcon aria-hidden className={cn("size-4", feature.soon ? "opacity-40" : "text-primary")} />
            {feature.label}
            {feature.soon ? <Badge variant="outline">Soon</Badge> : null}
          </li>
        ))}
        {name === "Pro" ? (
          <li className="flex items-center gap-2.5">
            <HeartIcon aria-hidden className="text-primary size-4 fill-current" />
            You’re supporting Ployz
          </li>
        ) : null}
      </ul>
      {children}
    </section>
  );
}

// UTC so the server render and the browser agree on the day.
const periodEndFormat = new Intl.DateTimeFormat("en-US", { dateStyle: "medium", timeZone: "UTC" });

function periodEndCopy(state: { currentPeriodEnd: Date | null; cancelAtPeriodEnd: boolean }) {
  if (state.currentPeriodEnd === null) return "Pro is active";
  const day = periodEndFormat.format(state.currentPeriodEnd);
  return state.cancelAtPeriodEnd ? `Pro ends on ${day}` : `Renews on ${day}`;
}
