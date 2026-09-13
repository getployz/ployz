import { CheckIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "#/components/ui/card";
import { Spinner } from "#/components/ui/spinner";
import type { BillingPlan } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/billing-plans";

export function BillingPlanCard({
  plan,
  ctaLabel,
  disabled,
  isCurrentPlan,
  isPending,
  onCheckout,
}: {
  plan: BillingPlan;
  ctaLabel: string;
  disabled: boolean;
  isCurrentPlan: boolean;
  isPending: boolean;
  onCheckout: () => void;
}) {
  return (
    <Card className="flex h-full flex-col">
      <CardHeader className="flex flex-col gap-3">
        <div className="flex gap-2">
          {plan.recommended ? <Badge>Recommended</Badge> : null}
          {isCurrentPlan ? <Badge variant="secondary">Current</Badge> : null}
        </div>
        <div className="flex flex-col gap-2">
          <CardTitle>{plan.name}</CardTitle>
          <CardDescription>{plan.description}</CardDescription>
        </div>
        <p className="text-4xl font-semibold tracking-tight">{plan.price}</p>
      </CardHeader>
      <CardContent className="flex flex-1 flex-col gap-3">
        {plan.features.map((feature) => (
          <div key={feature} className="flex items-start gap-3">
            <CheckIcon className="mt-0.5 text-primary" />
            <span className="text-muted-foreground">{feature}</span>
          </div>
        ))}
      </CardContent>
      <CardFooter>
        <Button
          className="w-full"
          variant={plan.ctaVariant}
          disabled={disabled}
          onClick={onCheckout}
        >
          {isPending ? <Spinner data-icon="inline-start" /> : null}
          {ctaLabel}
        </Button>
      </CardFooter>
    </Card>
  );
}
