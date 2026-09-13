export const billingPlans = [
  {
    slug: "free",
    name: "Free",
    description: "For experimenting and personal projects",
    price: "$0/mo",
    ctaLabel: "Choose Free",
    ctaVariant: "outline" as const,
    recommended: false,
    features: [
      "Unlimited servers",
      "Unlimited deployments",
      "Unlimited applications",
      "Unlimited databases",
      "2 environments",
      "No log storage",
      "No metrics retention",
      "No backups",
      "No scheduled jobs",
      "Community support",
    ],
  },
  {
    slug: "solo",
    name: "Solo",
    description: "For individual developers who want real operational tooling",
    price: "$9/mo",
    ctaLabel: "Get Started",
    ctaVariant: "default" as const,
    recommended: true,
    features: [
      "Everything in Free",
      "Log storage",
      "Metrics retention",
      "Volume backups",
      "Database backups",
      "Scheduled jobs",
      "More environments",
      "Email support",
    ],
  },
  {
    slug: "teams",
    name: "Teams",
    description: "For production workloads",
    price: "$29/mo",
    ctaLabel: "Get Started",
    ctaVariant: "outline" as const,
    recommended: false,
    features: [
      "Everything in Solo",
      "Unlimited environments",
      "Longer log and metric retention",
      "Unlimited backups",
      "Unlimited scheduled jobs",
      "Priority email and chat support",
    ],
  },
] as const;

export type BillingPlan = (typeof billingPlans)[number];
export type BillingPlanSlug = BillingPlan["slug"];

export function getBillingPlanName(slug: BillingPlanSlug) {
  return billingPlans.find((plan) => plan.slug === slug)?.name ?? slug;
}
