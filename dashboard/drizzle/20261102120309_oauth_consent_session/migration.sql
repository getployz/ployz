CREATE TABLE "oauth_consent_session" (
	"consent_id" uuid,
	"session_id" uuid,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"signed_out_at" timestamp with time zone,
	CONSTRAINT "oauth_consent_session_pkey" PRIMARY KEY("consent_id","session_id")
);
--> statement-breakpoint
CREATE INDEX "oauth_consent_session_session_id_idx" ON "oauth_consent_session" ("session_id");--> statement-breakpoint
ALTER TABLE "oauth_consent_session" ADD CONSTRAINT "oauth_consent_session_consent_id_oauth_consent_id_fkey" FOREIGN KEY ("consent_id") REFERENCES "oauth_consent"("id") ON DELETE CASCADE;--> statement-breakpoint
-- A grant made before this table acted only while the browser session its tokens name was live. Link each consent to the
-- live sessions its standing refresh tokens name. One left without a link keeps that old rule (oauthCaller).
INSERT INTO "oauth_consent_session" ("consent_id", "session_id")
SELECT DISTINCT "oauth_consent"."id", "session"."id"
FROM "oauth_refresh_token"
INNER JOIN "oauth_consent" ON "oauth_consent"."client_id" = "oauth_refresh_token"."client_id"
  AND "oauth_consent"."user_id" = "oauth_refresh_token"."user_id"
  AND "oauth_consent"."reference_id" = "oauth_refresh_token"."reference_id"
INNER JOIN "session" ON "session"."id" = "oauth_refresh_token"."session_id"
  AND "session"."user_id" = "oauth_refresh_token"."user_id"
WHERE "oauth_refresh_token"."revoked" IS NULL
  AND "oauth_refresh_token"."expires_at" > now()
  AND "session"."expires_at" > now()
  AND date_trunc('second', "oauth_refresh_token"."created_at") >= "oauth_consent"."created_at"
ON CONFLICT DO NOTHING;
