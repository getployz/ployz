import { Button } from "#/components/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "#/components/ui/dialog";
import { Spinner } from "#/components/ui/spinner";
import {
  getBillingPlanName,
  type BillingPlanSlug,
} from "#/routes/_protected/cloud/$organizationSlug/_org/-components/billing-plans";

export type BillingPlanChangePreview = {
  currentPlan: BillingPlanSlug;
  targetPlan: BillingPlanSlug;
  currency: string;
  currentAmount: number;
  targetAmount: number;
  estimatedDelta: number;
  remainingRatio: number;
  currentPeriodStart: Date;
  currentPeriodEnd: Date;
  prorationBehavior: "invoice";
};

export function BillingPlanChangeDialog({
  open,
  pendingPlan,
  preview,
  formatCurrency,
  onConfirm,
  onOpenChange,
}: {
  open: boolean;
  pendingPlan: string | null;
  preview: BillingPlanChangePreview | null;
  formatCurrency: (amount: number) => string;
  onConfirm: () => void;
  onOpenChange: (open: boolean) => void;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>Change plan</DialogTitle>
          <DialogDescription>
            Review the estimated charge before you confirm.
          </DialogDescription>
        </DialogHeader>

        {preview ? (
          <div className="flex flex-col gap-4 text-sm">
            <div className="flex items-center justify-between gap-3">
              <span className="text-muted-foreground">Plan change</span>
              <span className="font-medium">
                {getBillingPlanName(preview.currentPlan)} to{" "}
                {getBillingPlanName(preview.targetPlan)}
              </span>
            </div>
            <div className="flex items-center justify-between gap-3">
              <span className="text-muted-foreground">Current price</span>
              <span>{formatCurrency(preview.currentAmount)}</span>
            </div>
            <div className="flex items-center justify-between gap-3">
              <span className="text-muted-foreground">New price</span>
              <span>{formatCurrency(preview.targetAmount)}</span>
            </div>
            <div className="flex items-center justify-between gap-3">
              <span className="text-muted-foreground">Estimated due now</span>
              <span className="font-medium">
                {preview.estimatedDelta < 0 ? "-" : ""}
                {formatCurrency(Math.abs(preview.estimatedDelta))}
              </span>
            </div>
            <div className="flex items-center justify-between gap-3">
              <span className="text-muted-foreground">Billing period ends</span>
              <span>{preview.currentPeriodEnd.toLocaleDateString()}</span>
            </div>
            <p className="text-muted-foreground">
              The new price takes effect when you confirm. This estimate is
              based on the remaining{" "}
              {Math.round(preview.remainingRatio * 100)}% of your billing
              period. Taxes, discounts, and rounding can change the final amount.
            </p>
          </div>
        ) : null}

        <DialogFooter>
          <DialogClose render={<Button variant="outline" />}>Cancel</DialogClose>
          <Button
            disabled={preview === null || pendingPlan !== null}
            onClick={onConfirm}
          >
            {preview && pendingPlan === preview.targetPlan ? (
              <Spinner data-icon="inline-start" />
            ) : null}
            {preview && pendingPlan === preview.targetPlan
              ? "Changing plan…"
              : "Confirm change"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
