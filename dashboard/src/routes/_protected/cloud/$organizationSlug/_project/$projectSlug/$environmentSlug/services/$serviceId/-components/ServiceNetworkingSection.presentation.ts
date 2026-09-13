import type { BillingPlan } from "#/modules/billing/tables";

type CustomDomainBillingPresentationInput =
  | { status: "loading" }
  | { status: "unavailable" }
  | {
      status: "ready";
      billingMode: "hosted" | "self_hosted";
      currentPlan: BillingPlan | null;
      hasActivePaidSubscription: boolean;
    };

export type CustomDomainCapabilityPresentation =
  | {
      status: "allowed";
      canAddOrReplace: true;
      showUpgrade: false;
    }
  | {
      status: "blocked";
      canAddOrReplace: false;
      message: string;
      showUpgrade: boolean;
    };

/** Billing authorization for authored custom-domain edits. Runtime Watch data
 * never participates in this decision. */
export function customDomainCapabilityPresentation(
  input: CustomDomainBillingPresentationInput,
): CustomDomainCapabilityPresentation {
  if (input.status === "loading") {
    return {
      status: "blocked",
      canAddOrReplace: false,
      message: "Checking custom-domain access…",
      showUpgrade: false,
    };
  }
  if (input.status === "unavailable") {
    return {
      status: "blocked",
      canAddOrReplace: false,
      message: "We couldn’t check your plan. Try again.",
      showUpgrade: false,
    };
  }
  if (input.billingMode === "self_hosted") {
    return {
      status: "allowed",
      canAddOrReplace: true,
      showUpgrade: false,
    };
  }
  if (
    input.hasActivePaidSubscription &&
    (input.currentPlan === "solo" || input.currentPlan === "teams")
  ) {
    return {
      status: "allowed",
      canAddOrReplace: true,
      showUpgrade: false,
    };
  }
  if (input.currentPlan === "free") {
    return {
      status: "blocked",
      canAddOrReplace: false,
      message: "Custom domains are available on Solo and Teams.",
      showUpgrade: true,
    };
  }
  return {
    status: "blocked",
    canAddOrReplace: false,
    message: "We couldn’t check your plan. Try again.",
    showUpgrade: false,
  };
}
