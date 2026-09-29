CREATE TABLE "upload_chunk" (
	"deployment_id" text,
	"index" integer,
	"organization_id" uuid NOT NULL,
	"data" bytea NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "upload_chunk_pkey" PRIMARY KEY("deployment_id","index"),
	CONSTRAINT "upload_chunk_index_check" CHECK ("index" >= 0)
);
--> statement-breakpoint
CREATE INDEX "upload_chunk_created_at_idx" ON "upload_chunk" ("created_at");--> statement-breakpoint
ALTER TABLE "upload_chunk" ADD CONSTRAINT "upload_chunk_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;