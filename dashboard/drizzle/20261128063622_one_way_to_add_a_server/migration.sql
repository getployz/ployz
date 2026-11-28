-- Fleet Tokens, Waiting Servers and Server Enrollments go: every Server is added with its own install line.
-- Dropping the tables revokes every Fleet Token. Hosts still checking in get a stop from /api/checkin, which ends their loop.
-- A token Cloud minted for a Server Enrollment whose host never claimed it can't be used any more: its host's loop ends.
-- A claimed one stays, so a host already joining finishes.
UPDATE "machine_enrollment_token" SET "expires_at" = now(), "updated_at" = now()
  WHERE "server_enrollment_id" IS NOT NULL AND "claimed_machine_id" IS NULL AND "expires_at" > now();--> statement-breakpoint
ALTER TABLE "fleet_token_join" DROP CONSTRAINT "fleet_token_join_fleet_token_id_fleet_token_id_fkey";--> statement-breakpoint
ALTER TABLE "server_enrollment" DROP CONSTRAINT "server_enrollment_waiting_server_id_waiting_server_id_fkey";--> statement-breakpoint
ALTER TABLE "waiting_server" DROP CONSTRAINT "waiting_server_fleet_token_id_fleet_token_id_fkey";--> statement-breakpoint
DROP TABLE "fleet_token";--> statement-breakpoint
DROP TABLE "fleet_token_attempt";--> statement-breakpoint
DROP TABLE "fleet_token_join";--> statement-breakpoint
DROP TABLE "server_enrollment";--> statement-breakpoint
DROP FUNCTION "fleet_token_attempt_record"();--> statement-breakpoint
DROP TABLE "waiting_server";--> statement-breakpoint
ALTER TABLE "machine_enrollment_token" ADD COLUMN "ingress_closed" boolean DEFAULT false NOT NULL;--> statement-breakpoint
ALTER TABLE "machine_enrollment_token" DROP COLUMN "enrollment_role";--> statement-breakpoint
ALTER TABLE "machine_enrollment_token" DROP COLUMN "server_enrollment_id";