DELETE FROM "variable" WHERE "service_id" IS NULL;--> statement-breakpoint
DELETE FROM "environment_resource" WHERE "implementation_type" = 'variable_group';--> statement-breakpoint
DELETE FROM "environment_canvas_node_position" WHERE "resource_type" = 'variable_group';--> statement-breakpoint
DELETE FROM "environment_node_config_snapshot" WHERE "node_type" = 'variable_group';--> statement-breakpoint
DELETE FROM "environment_node_introduction" WHERE "node_type" = 'variable_group';--> statement-breakpoint
ALTER TABLE "environment_resource" DROP CONSTRAINT "environment_resource_nABwUGd5rp5C_fkey";--> statement-breakpoint
ALTER TABLE "environment_resource" DROP CONSTRAINT "environment_resource_s2Nqmux9EkpK_fkey";--> statement-breakpoint
ALTER TABLE "environment_variable_group" DROP CONSTRAINT "environment_variable_group_hiba8dNGKiwl_fkey";--> statement-breakpoint
ALTER TABLE "environment_variable_group" DROP CONSTRAINT "environment_variable_group_pbUjmr26aBQN_fkey";--> statement-breakpoint
ALTER TABLE "variable" DROP CONSTRAINT "variable_variable_group_id_environment_variable_group_id_fkey";--> statement-breakpoint
ALTER TABLE "variable" DROP CONSTRAINT "variable_AmBxCm84j9h8_fkey";--> statement-breakpoint
DROP TABLE "environment_variable_group";--> statement-breakpoint
DROP TABLE "variable_group_lineage";--> statement-breakpoint
ALTER TABLE "environment_resource" DROP CONSTRAINT "environment_resource_variable_group_reference_check";--> statement-breakpoint
ALTER TABLE "environment_resource" DROP CONSTRAINT "environment_resource_volume_no_variable_group_check";--> statement-breakpoint
ALTER TABLE "variable" DROP CONSTRAINT "variable_owner_check";--> statement-breakpoint
DROP INDEX "environment_resource_variable_group_variable_group_unique";--> statement-breakpoint
DROP INDEX "environment_resource_variable_group_id_idx";--> statement-breakpoint
ALTER TABLE "environment_resource" DROP COLUMN "variable_group_id";--> statement-breakpoint
ALTER TABLE "variable" DROP COLUMN "variable_group_id";--> statement-breakpoint
ALTER TABLE "variable" ALTER COLUMN "service_id" SET NOT NULL;--> statement-breakpoint
ALTER TABLE "environment_resource" DROP CONSTRAINT "environment_resource_implementation_type_check", ADD CONSTRAINT "environment_resource_implementation_type_check" CHECK ("implementation_type" in ('volume'));--> statement-breakpoint
ALTER TABLE "environment_node_config_snapshot" DROP CONSTRAINT "environment_node_config_snapshot_node_type_check", ADD CONSTRAINT "environment_node_config_snapshot_node_type_check" CHECK ("node_type" in ('service', 'volume'));--> statement-breakpoint
ALTER TABLE "environment_node_introduction" DROP CONSTRAINT "environment_node_introduction_node_type_check", ADD CONSTRAINT "environment_node_introduction_node_type_check" CHECK ("node_type" in ('service', 'volume'));