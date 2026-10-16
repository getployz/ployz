CREATE TABLE "checkin_rate_limit" (
	"key" text PRIMARY KEY,
	"window_start" timestamp with time zone NOT NULL,
	"count" integer NOT NULL
);
--> statement-breakpoint
CREATE TABLE "waiting_server" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"fleet_token_id" uuid NOT NULL,
	"checkin_key" text NOT NULL,
	"report" jsonb NOT NULL,
	"observed_ip" text NOT NULL,
	"probe" jsonb,
	"first_seen_at" timestamp with time zone DEFAULT now() NOT NULL,
	"last_check_in_at" timestamp with time zone DEFAULT now() NOT NULL,
	"rejected_at" timestamp with time zone
);
--> statement-breakpoint
CREATE UNIQUE INDEX "waiting_server_key_idx" ON "waiting_server" ("organization_id","checkin_key");--> statement-breakpoint
CREATE INDEX "waiting_server_fleet_token_idx" ON "waiting_server" ("fleet_token_id");--> statement-breakpoint
ALTER TABLE "waiting_server" ADD CONSTRAINT "waiting_server_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "waiting_server" ADD CONSTRAINT "waiting_server_fleet_token_id_fleet_token_id_fkey" FOREIGN KEY ("fleet_token_id") REFERENCES "fleet_token"("id") ON DELETE CASCADE;--> statement-breakpoint
SELECT organization_change_attach('waiting_server', 'organization_id', 'id');
