CREATE TABLE "device_request" (
	"organization_id" uuid,
	"code" text,
	"key" text NOT NULL,
	"expires_at" timestamp with time zone NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "device_request_pkey" PRIMARY KEY("organization_id","code"),
	CONSTRAINT "device_request_code_shape" CHECK ("code" ~ '^[a-z2-7]{10}$'),
	CONSTRAINT "device_request_key_shape" CHECK ("key" ~ '^[0-9a-f]{64}$')
);
--> statement-breakpoint
ALTER TABLE "device_request" ADD CONSTRAINT "device_request_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;