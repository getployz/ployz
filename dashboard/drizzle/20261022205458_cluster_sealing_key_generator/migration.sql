CREATE TABLE "cluster_sealing_key_generator" (
	"organization_id" uuid,
	"cluster_key" text,
	"machine_id" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "cluster_sealing_key_generator_pkey" PRIMARY KEY("organization_id","cluster_key"),
	CONSTRAINT "cluster_sealing_key_generator_cluster_key_check" CHECK ("cluster_key" ~ '^[0-9a-f]{64}$'),
	CONSTRAINT "cluster_sealing_key_generator_machine_id_check" CHECK ("machine_id" ~ '^[0-9a-f]{32}$')
);
--> statement-breakpoint
ALTER TABLE "cluster_sealing_key_generator" ADD CONSTRAINT "cluster_sealing_key_generator_hh64maQFnes8_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;