import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useServerFn } from "@tanstack/react-start";
import { ArrowDownIcon, CheckIcon, LockIcon, ZapIcon } from "lucide-react";
import { toast } from "sonner";
import { Button } from "#/components/ui/button";
import { Sheet, SheetContent, SheetDescription, SheetFooter, SheetHeader, SheetTitle } from "#/components/ui/sheet";
import { Spinner } from "#/components/ui/spinner";
import { PRO_PRICE } from "#/modules/billing/billing";
import { syncBillingAfterCheckoutServerFn } from "#/modules/billing/billing.functions";
import { billingKeys } from "#/modules/billing/billing.queries";
import { useEmbeddedCheckout } from "#/modules/billing/use-embedded-checkout";
import { capturePostHog } from "#/modules/analytics/posthog";

const PRO_PERKS = [
  { title: "HTTPS handled", detail: "Certificates issued and renewed for you." },
  { title: "As many as you need", detail: "Any domain or subdomain, on any service." },
  { title: "Point one DNS record", detail: "We show you exactly what to add." },
];

/** What "Custom Domain" opens when the Organization has no Pro: the pitch and Polar's checkout, not a form it would refuse. */
export function CustomDomainUpsellSheet({
  organizationSlug,
  service,
  environment,
  onUpgraded,
  onClose,
}: {
  organizationSlug: string;
  service: string;
  environment: string | null;
  /** Pro is on: go on to what the user came for. */
  onUpgraded: () => void;
  onClose: () => void;
}) {
  const checkout = useEmbeddedCheckout(organizationSlug);
  const syncBilling = useServerFn(syncBillingAfterCheckoutServerFn);
  const queryClient = useQueryClient();
  // Paid; Cloud is reading Pro from Polar. Custom domains follow from Pro on hosted Cloud.
  const [finishing, setFinishing] = useState(false);
  // The top of the paid funnel: checkout_started and subscription_started come from the server.
  useEffect(() => capturePostHog("custom_domain_upsell_viewed"), []);

  async function finishUpgrade() {
    setFinishing(true);
    let pro = false;
    try {
      pro = await syncBilling({ data: { organizationSlug } });
    } catch {
      // Polar's webhook still switches Pro on.
    }
    await queryClient.invalidateQueries({ queryKey: billingKeys.org(organizationSlug) });
    if (pro) {
      toast.success("You're on Pro. Thanks for supporting Ployz!");
      onUpgraded();
    } else {
      toast.success("Payment went through. Pro switches on in a moment.");
      onClose();
    }
  }

  return (
    <Sheet open onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="data-[side=right]:sm:max-w-md">
        <SheetHeader>
          <SheetTitle>Use your own domain</SheetTitle>
          <SheetDescription>
            Serve <span className="font-mono">{service}</span> from a domain you own.
          </SheetDescription>
        </SheetHeader>
        <div className="flex flex-col gap-6 px-4">
          <div className="flex flex-col items-center gap-2 rounded-xl border bg-muted p-4">
            <div className="flex w-full items-center gap-2 rounded-lg border bg-background px-3 py-2">
              <LockIcon className="size-4 text-success" />
              <span className="font-mono text-sm">api.yourcompany.com</span>
            </div>
            <ArrowDownIcon className="size-4 text-muted-foreground" />
            <div className="flex w-full items-center gap-2 rounded-lg border bg-background px-3 py-2">
              <ZapIcon className="size-4" />
              <span className="font-mono text-sm">{service}</span>
              {environment ? <span className="ml-auto text-muted-foreground text-sm">{environment}</span> : null}
            </div>
          </div>
          <ul className="flex flex-col gap-3">
            {PRO_PERKS.map((perk) => (
              <li key={perk.title} className="flex gap-3">
                <CheckIcon className="mt-0.5 size-4 shrink-0" />
                <div>
                  <div className="font-medium">{perk.title}</div>
                  <div className="text-muted-foreground text-sm">{perk.detail}</div>
                </div>
              </li>
            ))}
          </ul>
        </div>
        <SheetFooter className="border-t">
          <div className="flex items-baseline justify-between">
            <span className="text-muted-foreground">Ployz Pro</span>
            <span><span className="font-semibold">{PRO_PRICE}</span><span className="text-muted-foreground"> / month</span></span>
          </div>
          <Button type="button" size="lg" disabled={checkout.pending || finishing}
            onClick={() => void checkout.openCheckout({ onSuccess: () => void finishUpgrade() })}>
            {checkout.pending || finishing ? <Spinner data-icon="inline-start" /> : null}
            {finishing ? "Finishing your upgrade…" : "Upgrade to Pro"}
          </Button>
          <p className="text-center text-muted-foreground text-sm">Everything else stays free on your servers.</p>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}
