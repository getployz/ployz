CREATE TABLE "operation_outbox" (
	"event_id" text PRIMARY KEY,
	"name" text NOT NULL,
	"data" jsonb NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"sent_at" timestamp with time zone
);
--> statement-breakpoint
CREATE TABLE "operation_request" (
	"organization_id" uuid,
	"request_key" text,
	"tool" text NOT NULL,
	"digest" text NOT NULL,
	"attempt_id" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "operation_request_pkey" PRIMARY KEY("organization_id","request_key"),
	CONSTRAINT "operation_request_key_check" CHECK ("request_key" ~ '^[A-Za-z0-9_-]{8,64}$')
);
--> statement-breakpoint
ALTER TABLE "machine_remove_attempt" ADD COLUMN "remove_sent_at" timestamp with time zone;--> statement-breakpoint
CREATE INDEX "operation_outbox_unsent_idx" ON "operation_outbox" ("created_at") WHERE "sent_at" is null;--> statement-breakpoint
ALTER TABLE "operation_request" ADD CONSTRAINT "operation_request_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;