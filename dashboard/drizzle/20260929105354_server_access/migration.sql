CREATE TABLE "server_access" (
	"organization_id" uuid,
	"machine_id" text,
	"credential_id" uuid,
	"credential_kind" text NOT NULL,
	"user_id" uuid NOT NULL,
	"encrypted_capability" jsonb,
	"revoked_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "server_access_pkey" PRIMARY KEY("organization_id","credential_id","machine_id"),
	CONSTRAINT "server_access_credential_kind_check" CHECK ("credential_kind" in ('session', 'token')),
	CONSTRAINT "server_access_revoked_shape_check" CHECK (("revoked_at" is null) = ("encrypted_capability" is not null))
);
--> statement-breakpoint
CREATE INDEX "server_access_credential_idx" ON "server_access" ("credential_id");--> statement-breakpoint
ALTER TABLE "server_access" ADD CONSTRAINT "server_access_organization_machine_fkey" FOREIGN KEY ("organization_id","machine_id") REFERENCES "organization_machine"("organization_id","machine_id") ON DELETE CASCADE;