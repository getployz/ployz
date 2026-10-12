ALTER TABLE "organization_support_access" ADD COLUMN "variables" boolean DEFAULT false NOT NULL;--> statement-breakpoint
ALTER TABLE "organization_support_access" ADD COLUMN "configs" boolean DEFAULT false NOT NULL;--> statement-breakpoint
ALTER TABLE "organization_support_access" ADD COLUMN "logs" boolean DEFAULT false NOT NULL;--> statement-breakpoint
ALTER TABLE "organization_support_access" ADD CONSTRAINT "organization_support_access_shows_only_when_allowed" CHECK ("allowed" or not ("variables" or "configs" or "logs"));