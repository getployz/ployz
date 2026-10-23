CREATE TABLE "organization_sealing_key" (
	"organization_id" uuid PRIMARY KEY,
	"public_key" text NOT NULL,
	"recovery_blob" text,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "organization_sealing_key_public_key_check" CHECK ("public_key" ~ '^[A-Za-z0-9+/]{43}=$'),
	CONSTRAINT "organization_sealing_key_recovery_blob_check" CHECK ("recovery_blob" ~ '^[A-Za-z0-9+/]{103}=$')
);
--> statement-breakpoint
ALTER TABLE "organization_sealing_key" ADD CONSTRAINT "organization_sealing_key_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
INSERT INTO "organization_sealing_key" ("organization_id", "public_key")
SELECT "organization_id", min("public_key")
FROM "cluster_sealing_key"
GROUP BY "organization_id"
HAVING count(DISTINCT "public_key") = 1
ON CONFLICT DO NOTHING;
