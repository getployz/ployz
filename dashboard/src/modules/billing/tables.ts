import { organization } from "#/modules/organization/tables";

import { sql } from "drizzle-orm";

import { boolean, check, integer, pgTable, text, timestamp, uuid } from "drizzle-orm/pg-core";



export const BILLING_PLANS = ["free", "solo", "teams"] as const;

export type BillingPlan = (typeof BILLING_PLANS)[number];

export const STORED_BILLING_PLANS = [...BILLING_PLANS, "hobby", "pro"] as const;

export type StoredBillingPlan = (typeof STORED_BILLING_PLANS)[number];

export const organizationBillingState = pgTable(
  "organization_billing_state",
  {
    organizationId: uuid("organization_id")
      .primaryKey()
      .references(() => organization.id, { onDelete: "cascade" }),
    activeSubscriptionId: text("active_subscription_id"),
    currentPlan: text("current_plan").$type<StoredBillingPlan | null>(),
    productId: uuid("product_id"),
    amount: integer("amount"),
    currency: text("currency"),
    currentPeriodStart: timestamp("current_period_start", {
      mode: "date",
      withTimezone: true,
    }),
    currentPeriodEnd: timestamp("current_period_end", {
      mode: "date",
      withTimezone: true,
    }),
    hasActiveSubscription: boolean("has_active_subscription")
      .default(false)
      .notNull(),
    syncedAt: timestamp("synced_at", {
      mode: "date",
      withTimezone: true,
    })
      .defaultNow()
      .notNull(),
    sourceUpdatedAt: timestamp("source_updated_at", {
      mode: "date",
      withTimezone: true,
    }),
  },
  (table) => [
    check(
      "organization_billing_state_active_fields_check",
      sql`(
        ${table.hasActiveSubscription} = false
        OR (
          ${table.activeSubscriptionId} IS NOT NULL
          AND ${table.currentPlan} IS NOT NULL
          AND ${table.productId} IS NOT NULL
          AND ${table.amount} IS NOT NULL
          AND ${table.currency} IS NOT NULL
          AND ${table.currentPeriodStart} IS NOT NULL
          AND ${table.currentPeriodEnd} IS NOT NULL
        )
      )`,
    ),
    check(
      "organization_billing_state_current_plan_check",
      sql`${table.currentPlan} IS NULL OR ${table.currentPlan} IN ('free', 'solo', 'teams', 'hobby', 'pro')`,
    ),
  ],
);
