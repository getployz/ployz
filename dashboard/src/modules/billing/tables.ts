import { organization } from "#/modules/organization/tables";

import { sql } from "drizzle-orm";

import { boolean, check, pgTable, text, timestamp, uuid } from "drizzle-orm/pg-core";

/** Hosted only: a Self-hosted Cloud never writes a row. */
export const organizationBillingState = pgTable(
  "organization_billing_state",
  {
    organizationId: uuid("organization_id")
      .primaryKey()
      .references(() => organization.id, { onDelete: "cascade" }),
    activeSubscriptionId: text("active_subscription_id"),
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
          AND ${table.currentPeriodEnd} IS NOT NULL
        )
      )`,
    ),
  ],
);
