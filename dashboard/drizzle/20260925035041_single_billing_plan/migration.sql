ALTER TABLE "organization_billing_state" DROP CONSTRAINT "organization_billing_state_current_plan_check";--> statement-breakpoint
ALTER TABLE "organization_billing_state" DROP CONSTRAINT "organization_billing_state_active_fields_check";--> statement-breakpoint
ALTER TABLE "organization_billing_state" DROP COLUMN "current_plan";--> statement-breakpoint
ALTER TABLE "organization_billing_state" DROP COLUMN "product_id";--> statement-breakpoint
ALTER TABLE "organization_billing_state" DROP COLUMN "amount";--> statement-breakpoint
ALTER TABLE "organization_billing_state" DROP COLUMN "currency";--> statement-breakpoint
ALTER TABLE "organization_billing_state" DROP COLUMN "current_period_start";--> statement-breakpoint
ALTER TABLE "organization_billing_state" ADD CONSTRAINT "organization_billing_state_active_fields_check" CHECK ((
        "has_active_subscription" = false
        OR (
          "active_subscription_id" IS NOT NULL
          AND "current_period_end" IS NOT NULL
        )
      ));
