CREATE TABLE "enrollment_allocation" (
	"organization_id" uuid,
	"cluster_key" text,
	"assignments" jsonb NOT NULL,
	CONSTRAINT "enrollment_allocation_pkey" PRIMARY KEY("organization_id","cluster_key"),
	CONSTRAINT "enrollment_allocation_cluster_key_check" CHECK ("cluster_key" ~ '^[0-9a-f]{64}$'),
	CONSTRAINT "enrollment_allocation_assignments_check" CHECK (jsonb_typeof("assignments") = 'array')
);
--> statement-breakpoint
ALTER TABLE "enrollment_allocation" ADD CONSTRAINT "enrollment_allocation_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;