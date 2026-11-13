CREATE TABLE "passkey_request" (
	"organization_id" uuid,
	"code" text,
	"user_id" uuid NOT NULL,
	"credential_id" text NOT NULL,
	"algorithm" text NOT NULL,
	"public_key" text NOT NULL,
	"account" text NOT NULL,
	"expires_at" timestamp with time zone NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "passkey_request_pkey" PRIMARY KEY("organization_id","code"),
	CONSTRAINT "passkey_request_code_shape" CHECK ("code" ~ '^[a-z2-7]{10}$'),
	CONSTRAINT "passkey_request_algorithm" CHECK ("algorithm" in ('es256', 'eddsa')),
	CONSTRAINT "passkey_request_key_shape" CHECK ("public_key" ~ '^[A-Za-z0-9_-]+$')
);
--> statement-breakpoint
CREATE INDEX "passkey_request_user" ON "passkey_request" ("organization_id","user_id");--> statement-breakpoint
ALTER TABLE "passkey_request" ADD CONSTRAINT "passkey_request_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "passkey_request" ADD CONSTRAINT "passkey_request_user_id_user_id_fkey" FOREIGN KEY ("user_id") REFERENCES "user"("id") ON DELETE CASCADE;