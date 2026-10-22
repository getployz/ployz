CREATE TABLE "cluster_sealing_key" (
	"organization_id" uuid,
	"cluster_key" text,
	"public_key" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "cluster_sealing_key_pkey" PRIMARY KEY("organization_id","cluster_key"),
	CONSTRAINT "cluster_sealing_key_cluster_key_check" CHECK ("cluster_key" ~ '^[0-9a-f]{64}$'),
	CONSTRAINT "cluster_sealing_key_public_key_check" CHECK ("public_key" ~ '^[A-Za-z0-9+/]{43}=$')
);
--> statement-breakpoint
ALTER TABLE "cluster_sealing_key" ADD CONSTRAINT "cluster_sealing_key_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;