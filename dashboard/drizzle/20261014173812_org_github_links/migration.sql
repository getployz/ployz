CREATE TABLE "organization_github_installation" (
	"organization_id" uuid,
	"installation_id" integer,
	"connected_by_user_id" uuid,
	"connected_github_login" text NOT NULL,
	"connected_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "organization_github_installation_pkey" PRIMARY KEY("organization_id","installation_id")
);
--> statement-breakpoint
INSERT INTO "organization_github_installation" ("organization_id", "installation_id", "connected_by_user_id", "connected_github_login", "connected_at")
SELECT DISTINCT ON (m."organization_id", gi."installation_id") m."organization_id", gi."installation_id", gi."user_id", gi."account_login", gi."created_at"
FROM "github_installation" gi
JOIN "member" m ON m."user_id" = gi."user_id" AND m."role" = 'owner'
ORDER BY m."organization_id", gi."installation_id", gi."created_at", gi."id"
ON CONFLICT DO NOTHING;--> statement-breakpoint
DELETE FROM "github_installation" gi
USING "github_installation" first
WHERE first."installation_id" = gi."installation_id" AND (first."created_at", first."id") < (gi."created_at", gi."id");--> statement-breakpoint
DELETE FROM "github_repository_cache" c
USING "github_repository_cache" newest
WHERE newest."installation_id" = c."installation_id" AND newest."repository_id" = c."repository_id" AND (newest."synced_at", newest."user_id") > (c."synced_at", c."user_id");--> statement-breakpoint
DELETE FROM "github_repository_cache" c
WHERE NOT EXISTS (SELECT 1 FROM "github_installation" gi WHERE gi."installation_id" = c."installation_id");--> statement-breakpoint
ALTER TABLE "github_repository_cache" DROP CONSTRAINT "github_repository_cache_user_id_user_id_fkey";--> statement-breakpoint
ALTER TABLE "github_installation" DROP CONSTRAINT "github_installation_user_id_user_id_fkey";--> statement-breakpoint
ALTER TABLE "github_installation" DROP CONSTRAINT "github_installation_user_id_installation_id_unique";--> statement-breakpoint
DROP INDEX "github_installation_user_idx";--> statement-breakpoint
DROP INDEX "github_installation_installation_id_idx";--> statement-breakpoint
DROP INDEX "github_repository_cache_installation_idx";--> statement-breakpoint
DROP INDEX "github_repository_cache_user_idx";--> statement-breakpoint
ALTER TABLE "github_installation" DROP COLUMN "id";--> statement-breakpoint
ALTER TABLE "github_installation" DROP COLUMN "user_id";--> statement-breakpoint
ALTER TABLE "github_repository_cache" DROP COLUMN "user_id";--> statement-breakpoint
ALTER TABLE "github_repository_cache" ADD PRIMARY KEY ("installation_id","repository_id");--> statement-breakpoint
ALTER TABLE "github_installation" ADD PRIMARY KEY ("installation_id");--> statement-breakpoint
CREATE INDEX "organization_github_installation_installation_idx" ON "organization_github_installation" ("installation_id");--> statement-breakpoint
ALTER TABLE "github_repository_cache" ADD CONSTRAINT "github_repository_cache_1YBtmuU8cDTC_fkey" FOREIGN KEY ("installation_id") REFERENCES "github_installation"("installation_id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "organization_github_installation" ADD CONSTRAINT "organization_github_installation_SbYu7cqZzW9M_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "organization_github_installation" ADD CONSTRAINT "organization_github_installation_aAoZjPsdZFh5_fkey" FOREIGN KEY ("installation_id") REFERENCES "github_installation"("installation_id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "organization_github_installation" ADD CONSTRAINT "organization_github_installation_dG3N0i4nA3Do_fkey" FOREIGN KEY ("connected_by_user_id") REFERENCES "user"("id") ON DELETE SET NULL;--> statement-breakpoint
SELECT organization_change_attach('organization_github_installation', 'organization_id', 'installation_id');--> statement-breakpoint
ALTER TABLE "github_installation" ADD COLUMN "suspended_at" timestamp with time zone;
