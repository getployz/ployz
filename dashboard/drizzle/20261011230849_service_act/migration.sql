CREATE TABLE "service_act" (
	"id" uuid PRIMARY KEY,
	"organization_id" uuid NOT NULL,
	"environment_id" text NOT NULL,
	"service" text NOT NULL,
	"namespace" text NOT NULL,
	"runtime" text NOT NULL,
	"verb" text NOT NULL,
	"only" jsonb,
	"digest" text NOT NULL,
	"state" text DEFAULT 'accepted' NOT NULL,
	"before" jsonb,
	"legs" jsonb DEFAULT '[]' NOT NULL,
	"reason" text,
	"inngest_run_id" text,
	"requested_by_user_id" uuid,
	"requested_at" timestamp with time zone DEFAULT now() NOT NULL,
	"started_at" timestamp with time zone,
	"ended_at" timestamp with time zone,
	CONSTRAINT "service_act_state_check" CHECK ("state" in ('accepted','running','done','failed','unknown')),
	CONSTRAINT "service_act_verb_check" CHECK ("verb" in ('restart','start','stop')),
	CONSTRAINT "service_act_legs_check" CHECK (jsonb_typeof("legs") = 'array'),
	CONSTRAINT "service_act_reason_check" CHECK ("reason" is null or length("reason") between 1 and 1024),
	CONSTRAINT "service_act_state_shape_check" CHECK ((
        ("state" = 'accepted' and "started_at" is null and "ended_at" is null)
        or ("state" = 'running' and "inngest_run_id" is not null
          and "started_at" is not null and "ended_at" is null and "before" is not null)
        or ("state" in ('done', 'failed', 'unknown') and "ended_at" is not null)
      ))
);
--> statement-breakpoint
CREATE UNIQUE INDEX "service_act_one_active_idx" ON "service_act" ("organization_id","environment_id","runtime") WHERE "state" in ('accepted', 'running');--> statement-breakpoint
CREATE UNIQUE INDEX "service_act_run_uidx" ON "service_act" ("inngest_run_id") WHERE "inngest_run_id" is not null;--> statement-breakpoint
CREATE INDEX "service_act_latest_idx" ON "service_act" ("organization_id","environment_id","service","requested_at" DESC NULLS LAST);--> statement-breakpoint
ALTER TABLE "service_act" ADD CONSTRAINT "service_act_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "service_act" ADD CONSTRAINT "service_act_requested_by_user_id_user_id_fkey" FOREIGN KEY ("requested_by_user_id") REFERENCES "user"("id") ON DELETE SET NULL;