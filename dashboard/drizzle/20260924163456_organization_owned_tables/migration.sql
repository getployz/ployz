-- Backfill each row's Organization from its parent row, then require it.
ALTER TABLE "service_lineage" ADD COLUMN "organization_id" uuid;--> statement-breakpoint
ALTER TABLE "service_registry_credential" ADD COLUMN "organization_id" uuid;--> statement-breakpoint
ALTER TABLE "variable" ADD COLUMN "organization_id" uuid;--> statement-breakpoint
ALTER TABLE "variable_secret" ADD COLUMN "organization_id" uuid;--> statement-breakpoint
ALTER TABLE "environment_deployment_build_output" ADD COLUMN "organization_id" uuid;--> statement-breakpoint
ALTER TABLE "environment_deployment_build_step" ADD COLUMN "organization_id" uuid;--> statement-breakpoint
ALTER TABLE "environment_deployment_event" ADD COLUMN "organization_id" uuid;--> statement-breakpoint
ALTER TABLE "environment_deployment_secret" ADD COLUMN "organization_id" uuid;--> statement-breakpoint
ALTER TABLE "core_operation_event" ADD COLUMN "organization_id" uuid;--> statement-breakpoint
ALTER TABLE "environment_node_config_snapshot_secret" ADD COLUMN "organization_id" uuid;--> statement-breakpoint
ALTER TABLE "environment_node_introduction_secret" ADD COLUMN "organization_id" uuid;--> statement-breakpoint
ALTER TABLE "github_environment_trigger" ADD COLUMN "organization_id" uuid;--> statement-breakpoint
UPDATE "service_lineage" AS t SET "organization_id" = p."organization_id" FROM "project" AS p WHERE t.project_id = p.id;--> statement-breakpoint
UPDATE "service_registry_credential" AS t SET "organization_id" = p."organization_id" FROM "service" AS p WHERE t.service_id = p.id;--> statement-breakpoint
UPDATE "variable" AS t SET "organization_id" = p."organization_id" FROM "environment" AS p WHERE t.environment_id = p.id;--> statement-breakpoint
UPDATE "variable_secret" AS t SET "organization_id" = p."organization_id" FROM "environment" AS p WHERE t.environment_id = p.id;--> statement-breakpoint
UPDATE "environment_deployment_build_output" AS t SET "organization_id" = p."organization_id" FROM "environment_deployment" AS p WHERE t.deployment_id = p.id;--> statement-breakpoint
UPDATE "environment_deployment_build_step" AS t SET "organization_id" = p."organization_id" FROM "environment_deployment" AS p WHERE t.deployment_id = p.id;--> statement-breakpoint
UPDATE "environment_deployment_event" AS t SET "organization_id" = p."organization_id" FROM "environment_deployment" AS p WHERE t.deployment_id = p.id;--> statement-breakpoint
UPDATE "environment_deployment_secret" AS t SET "organization_id" = p."organization_id" FROM "environment_deployment" AS p WHERE t.environment_deployment_id = p.id;--> statement-breakpoint
UPDATE "core_operation_event" AS t SET "organization_id" = p."organization_id" FROM "core_operation_watch" AS p WHERE t.watch_id = p.id;--> statement-breakpoint
UPDATE "environment_node_config_snapshot_secret" AS t SET "organization_id" = p."organization_id" FROM "environment_node_config_snapshot" AS p WHERE t.snapshot_id = p.id;--> statement-breakpoint
UPDATE "environment_node_introduction_secret" AS t SET "organization_id" = p."organization_id" FROM "environment_node_introduction" AS p WHERE (t.environment_id, t.node_type, t.node_id) = (p.environment_id, p.node_type, p.node_id);--> statement-breakpoint
UPDATE "github_environment_trigger" AS t SET "organization_id" = p."organization_id" FROM "environment" AS p WHERE t.environment_id = p.id;--> statement-breakpoint
ALTER TABLE "service_lineage" ALTER COLUMN "organization_id" SET NOT NULL;--> statement-breakpoint
ALTER TABLE "service_registry_credential" ALTER COLUMN "organization_id" SET NOT NULL;--> statement-breakpoint
ALTER TABLE "variable" ALTER COLUMN "organization_id" SET NOT NULL;--> statement-breakpoint
ALTER TABLE "variable_secret" ALTER COLUMN "organization_id" SET NOT NULL;--> statement-breakpoint
ALTER TABLE "environment_deployment_build_output" ALTER COLUMN "organization_id" SET NOT NULL;--> statement-breakpoint
ALTER TABLE "environment_deployment_build_step" ALTER COLUMN "organization_id" SET NOT NULL;--> statement-breakpoint
ALTER TABLE "environment_deployment_event" ALTER COLUMN "organization_id" SET NOT NULL;--> statement-breakpoint
ALTER TABLE "environment_deployment_secret" ALTER COLUMN "organization_id" SET NOT NULL;--> statement-breakpoint
ALTER TABLE "core_operation_event" ALTER COLUMN "organization_id" SET NOT NULL;--> statement-breakpoint
ALTER TABLE "environment_node_config_snapshot_secret" ALTER COLUMN "organization_id" SET NOT NULL;--> statement-breakpoint
ALTER TABLE "environment_node_introduction_secret" ALTER COLUMN "organization_id" SET NOT NULL;--> statement-breakpoint
ALTER TABLE "github_environment_trigger" ALTER COLUMN "organization_id" SET NOT NULL;--> statement-breakpoint
ALTER TABLE "service_lineage" ADD CONSTRAINT "service_lineage_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "service_registry_credential" ADD CONSTRAINT "service_registry_credential_fC7nGiZZPNsV_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "variable" ADD CONSTRAINT "variable_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "variable_secret" ADD CONSTRAINT "variable_secret_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_deployment_build_output" ADD CONSTRAINT "environment_deployment_build_output_es7jBaLp3vH8_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_deployment_build_step" ADD CONSTRAINT "environment_deployment_build_step_28D6Rkzs1URj_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_deployment_event" ADD CONSTRAINT "environment_deployment_event_eW8RHtn0mFyI_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_deployment_secret" ADD CONSTRAINT "environment_deployment_secret_IuNsdKp3NB8E_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "core_operation_event" ADD CONSTRAINT "core_operation_event_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_node_config_snapshot_secret" ADD CONSTRAINT "environment_node_config_snapshot_secret_jZo1GvXOHIs5_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_node_introduction_secret" ADD CONSTRAINT "environment_node_introduction_secret_HaNAhrDo0dtU_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "github_environment_trigger" ADD CONSTRAINT "github_environment_trigger_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;