CREATE TABLE "notification_preference" (
	"user_id" uuid,
	"type" text,
	"channel" text NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "notification_preference_pkey" PRIMARY KEY("user_id","type"),
	CONSTRAINT "notification_preference_type_check" CHECK ("type" in ('uptime','deploy-failed','upgrade-failed')),
	CONSTRAINT "notification_preference_channel_check" CHECK ("channel" in ('email','none'))
);
--> statement-breakpoint
ALTER TABLE "notification_preference" ADD CONSTRAINT "notification_preference_user_id_user_id_fkey" FOREIGN KEY ("user_id") REFERENCES "user"("id") ON DELETE CASCADE;