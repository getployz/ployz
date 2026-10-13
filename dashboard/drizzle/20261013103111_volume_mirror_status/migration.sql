CREATE TABLE "volume_mirror_status" (
	"organization_id" uuid NOT NULL,
	"volume_id" text,
	"machine_id" text,
	"ok_at" timestamp with time zone,
	"ok_snapshot" text,
	"matched_at" timestamp with time zone,
	"failed_at" timestamp with time zone,
	"failed_snapshot" text,
	"failure" text,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "volume_mirror_status_pkey" PRIMARY KEY("volume_id","machine_id")
);
--> statement-breakpoint
ALTER TABLE "volume_mirror_status" ADD CONSTRAINT "volume_mirror_status_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;