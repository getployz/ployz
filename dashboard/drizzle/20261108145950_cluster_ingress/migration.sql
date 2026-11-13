CREATE TABLE "ingress_doors_attempt" (
	"id" uuid PRIMARY KEY,
	"organization_id" uuid NOT NULL,
	"requested_by_user_id" uuid,
	"want" text NOT NULL,
	"state" text DEFAULT 'pending' NOT NULL,
	"stage" text,
	"inngest_run_id" text,
	"failure" text,
	"requested_at" timestamp with time zone DEFAULT now() NOT NULL,
	"started_at" timestamp with time zone,
	"ended_at" timestamp with time zone,
	CONSTRAINT "ingress_doors_attempt_want_check" CHECK ("want" in ('tunnel','public')),
	CONSTRAINT "ingress_doors_attempt_state_check" CHECK ("state" in ('pending','running','finished','failed')),
	CONSTRAINT "ingress_doors_attempt_stage_check" CHECK ("stage" is null or "stage" in ('opening','moving','closing')),
	CONSTRAINT "ingress_doors_attempt_failure_check" CHECK (("state" = 'failed') = ("failure" is not null)
      and ("failure" is null or length("failure") between 1 and 1024)),
	CONSTRAINT "ingress_doors_attempt_run_check" CHECK ("inngest_run_id" is null or length("inngest_run_id") between 1 and 255)
);
--> statement-breakpoint
CREATE TABLE "organization_cloudflare_ingress" (
	"organization_id" uuid PRIMARY KEY,
	"zone" text NOT NULL,
	"tunnel_id" text,
	"tunnel_name" text NOT NULL,
	"state" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "organization_cloudflare_ingress_state_check" CHECK ("state" in ('setting_up','set_up','active','left')),
	CONSTRAINT "organization_cloudflare_ingress_tunnel_check" CHECK (("state" = 'setting_up') = ("tunnel_id" is null)),
	CONSTRAINT "organization_cloudflare_ingress_zone_check" CHECK ("zone" ~ '^[a-z0-9.-]+\.[a-z0-9-]+$')
);
--> statement-breakpoint
CREATE UNIQUE INDEX "ingress_doors_attempt_one_active_idx" ON "ingress_doors_attempt" ("organization_id") WHERE "state" in ('pending', 'running');--> statement-breakpoint
CREATE UNIQUE INDEX "ingress_doors_attempt_run_uidx" ON "ingress_doors_attempt" ("inngest_run_id") WHERE "inngest_run_id" is not null;--> statement-breakpoint
CREATE INDEX "ingress_doors_attempt_latest_idx" ON "ingress_doors_attempt" ("organization_id","requested_at");--> statement-breakpoint
ALTER TABLE "ingress_doors_attempt" ADD CONSTRAINT "ingress_doors_attempt_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "ingress_doors_attempt" ADD CONSTRAINT "ingress_doors_attempt_requested_by_user_id_user_id_fkey" FOREIGN KEY ("requested_by_user_id") REFERENCES "user"("id") ON DELETE SET NULL;--> statement-breakpoint
ALTER TABLE "organization_cloudflare_ingress" ADD CONSTRAINT "organization_cloudflare_ingress_O6Llyp8JvHjz_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;
--> statement-breakpoint
SELECT organization_change_attach('organization_cloudflare_ingress', 'organization_id', 'organization_id');--> statement-breakpoint
SELECT organization_change_attach('ingress_doors_attempt', 'organization_id', 'id');
