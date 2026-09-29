ALTER TABLE "environment" DROP CONSTRAINT "environment_project_id_project_id_fkey";--> statement-breakpoint
ALTER TABLE "environment" DROP CONSTRAINT "environment_IqLeTQthFKCX_fkey";--> statement-breakpoint
ALTER TABLE "environment_branch" DROP CONSTRAINT "environment_branch_environment_fkey";--> statement-breakpoint
ALTER TABLE "environment_branch" DROP CONSTRAINT "environment_branch_parent_fkey";--> statement-breakpoint
ALTER TABLE "project" DROP CONSTRAINT "project_default_environment_id_environment_id_fkey";--> statement-breakpoint
ALTER TABLE "environment_resource" DROP CONSTRAINT "environment_resource_project_id_project_id_fkey";--> statement-breakpoint
ALTER TABLE "environment_resource" DROP CONSTRAINT "environment_resource_environment_id_environment_id_fkey";--> statement-breakpoint
ALTER TABLE "environment_resource" DROP CONSTRAINT "environment_resource_lineage_id_resource_lineage_id_fkey";--> statement-breakpoint
ALTER TABLE "environment_resource" DROP CONSTRAINT "environment_resource_krkfQwOLhWDI_fkey";--> statement-breakpoint
ALTER TABLE "environment_resource" DROP CONSTRAINT "environment_resource_zQ2aoOGRLIOR_fkey";--> statement-breakpoint
ALTER TABLE "resource_lineage" DROP CONSTRAINT "resource_lineage_project_id_project_id_fkey";--> statement-breakpoint
ALTER TABLE "service" DROP CONSTRAINT "service_project_id_project_id_fkey";--> statement-breakpoint
ALTER TABLE "service" DROP CONSTRAINT "service_environment_id_environment_id_fkey";--> statement-breakpoint
ALTER TABLE "service" DROP CONSTRAINT "service_lineage_id_service_lineage_id_fkey";--> statement-breakpoint
ALTER TABLE "service" DROP CONSTRAINT "service_bvUAv6STak07_fkey";--> statement-breakpoint
ALTER TABLE "service" DROP CONSTRAINT "service_1xRYD1MXFhcT_fkey";--> statement-breakpoint
ALTER TABLE "service_lineage" DROP CONSTRAINT "service_lineage_project_id_project_id_fkey";--> statement-breakpoint
ALTER TABLE "service_registry_credential" DROP CONSTRAINT "service_registry_credential_service_id_service_id_fkey";--> statement-breakpoint
ALTER TABLE "variable" DROP CONSTRAINT "variable_environment_id_environment_id_fkey";--> statement-breakpoint
ALTER TABLE "variable" DROP CONSTRAINT "variable_service_id_service_id_fkey";--> statement-breakpoint
ALTER TABLE "variable" DROP CONSTRAINT "variable_dFGv8UXjv6eg_fkey";--> statement-breakpoint
ALTER TABLE "variable_secret" DROP CONSTRAINT "variable_secret_environment_id_environment_id_fkey";--> statement-breakpoint
ALTER TABLE "variable_secret" DROP CONSTRAINT "variable_secret_fgbg4UfEzo6H_fkey";--> statement-breakpoint
ALTER TABLE "environment_deployment" DROP CONSTRAINT "environment_deployment_environment_id_environment_id_fkey";--> statement-breakpoint
ALTER TABLE "environment_deployment" DROP CONSTRAINT "environment_deployment_92k6WdAbc3AZ_fkey";--> statement-breakpoint
ALTER TABLE "environment_deployment_build_output" DROP CONSTRAINT "environment_deployment_build_output_Xp68IrJouNSa_fkey";--> statement-breakpoint
ALTER TABLE "environment_deployment_build_output" DROP CONSTRAINT "environment_deployment_build_output_XF27nu5BhEx1_fkey";--> statement-breakpoint
ALTER TABLE "environment_deployment_build_step" DROP CONSTRAINT "environment_deployment_build_step_mAOz3YRgkCxT_fkey";--> statement-breakpoint
ALTER TABLE "environment_deployment_event" DROP CONSTRAINT "environment_deployment_event_nkhfxU66kCFO_fkey";--> statement-breakpoint
ALTER TABLE "environment_deployment_image_build" DROP CONSTRAINT "environment_deployment_image_build_Ss4XYtuD2fFu_fkey";--> statement-breakpoint
ALTER TABLE "environment_deployment_secret" DROP CONSTRAINT "environment_deployment_secret_IIXCU5ibjYPq_fkey";--> statement-breakpoint
ALTER TABLE "environment_saved_state_snapshot" DROP CONSTRAINT "environment_saved_state_snapshot_pYS3zmT4RKuQ_fkey";--> statement-breakpoint
ALTER TABLE "core_operation_event" DROP CONSTRAINT "core_operation_event_watch_id_core_operation_watch_id_fkey";--> statement-breakpoint
ALTER TABLE "environment_node_config_snapshot" DROP CONSTRAINT "environment_node_config_snapshot_CUaPaujMBOIs_fkey";--> statement-breakpoint
ALTER TABLE "environment_node_config_snapshot" DROP CONSTRAINT "environment_node_config_snapshot_D4DPm1gvv2Di_fkey";--> statement-breakpoint
ALTER TABLE "environment_node_config_snapshot_secret" DROP CONSTRAINT "environment_node_config_snapshot_secret_j5H7VPFuELrf_fkey";--> statement-breakpoint
ALTER TABLE "environment_node_introduction" DROP CONSTRAINT "environment_node_introduction_YPW21fX4uIUZ_fkey";--> statement-breakpoint
ALTER TABLE "environment_node_introduction_secret" DROP CONSTRAINT "environment_node_introduction_secret_sC7ALWFOnE38_fkey";--> statement-breakpoint
ALTER TABLE "volume_remove_attempt" DROP CONSTRAINT "volume_remove_attempt_environment_id_environment_id_fkey";--> statement-breakpoint
ALTER TABLE "volume_remove_attempt" DROP CONSTRAINT "volume_remove_attempt_2ncjupSSetid_fkey";--> statement-breakpoint
ALTER TABLE "github_branch_projection" DROP CONSTRAINT "github_branch_projection_Udo5xaDQmVxR_fkey";--> statement-breakpoint
ALTER TABLE "github_check_suite_projection" DROP CONSTRAINT "github_check_suite_projection_9FLSHaRf6WgI_fkey";--> statement-breakpoint
ALTER TABLE "github_environment_trigger" DROP CONSTRAINT "github_environment_trigger_environment_id_environment_id_fkey";--> statement-breakpoint
ALTER TABLE "github_environment_trigger" DROP CONSTRAINT "github_environment_trigger_dspFptTlur3h_fkey";--> statement-breakpoint
ALTER TABLE "conditional_save" DROP CONSTRAINT "conditional_save_pr_environment_id_environment_id_fkey";--> statement-breakpoint
ALTER TABLE "conditional_save" DROP CONSTRAINT "conditional_save_destination_environment_id_environment_id_fkey";--> statement-breakpoint
ALTER TABLE "conditional_save" DROP CONSTRAINT "conditional_save_project_fkey";--> statement-breakpoint
ALTER TABLE "pr_environment" DROP CONSTRAINT "pr_environment_branch_fkey";--> statement-breakpoint
ALTER TABLE "pr_environment_plan" DROP CONSTRAINT "pr_environment_plan_start_from_fkey";--> statement-breakpoint
ALTER TABLE "pr_environment_plan" DROP CONSTRAINT "pr_environment_plan_project_fkey";--> statement-breakpoint
DROP TABLE "environment";--> statement-breakpoint
DROP TABLE "environment_branch";--> statement-breakpoint
DROP TABLE "project";--> statement-breakpoint
DROP TABLE "environment_resource";--> statement-breakpoint
DROP TABLE "resource_lineage";--> statement-breakpoint
DROP TABLE "service";--> statement-breakpoint
DROP TABLE "service_lineage";--> statement-breakpoint
DROP TABLE "service_registry_credential";--> statement-breakpoint
DROP TABLE "variable";--> statement-breakpoint
DROP TABLE "variable_secret";--> statement-breakpoint
DROP TABLE "environment_deployment";--> statement-breakpoint
DROP TABLE "environment_deployment_build_output";--> statement-breakpoint
DROP TABLE "environment_deployment_build_step";--> statement-breakpoint
DROP TABLE "environment_deployment_event";--> statement-breakpoint
DROP TABLE "environment_deployment_image_build";--> statement-breakpoint
DROP TABLE "environment_deployment_secret";--> statement-breakpoint
DROP TABLE "environment_saved_state_snapshot";--> statement-breakpoint
DROP TABLE "organization_build_order";--> statement-breakpoint
DROP TABLE "core_operation_event";--> statement-breakpoint
DROP TABLE "core_operation_watch";--> statement-breakpoint
DROP TABLE "environment_node_config_snapshot";--> statement-breakpoint
DROP TABLE "environment_node_config_snapshot_secret";--> statement-breakpoint
DROP TABLE "environment_node_introduction";--> statement-breakpoint
DROP TABLE "environment_node_introduction_secret";--> statement-breakpoint
DROP TABLE "teardown_attempt";--> statement-breakpoint
DROP TABLE "volume_remove_attempt";--> statement-breakpoint
DROP TABLE "github_branch_projection";--> statement-breakpoint
DROP TABLE "github_check_suite_projection";--> statement-breakpoint
DROP TABLE "github_environment_trigger";--> statement-breakpoint
DROP TABLE "github_webhook_delivery";--> statement-breakpoint
DROP TABLE "conditional_save";--> statement-breakpoint
DROP TABLE "pr_environment";--> statement-breakpoint
DROP TABLE "pr_environment_plan";--> statement-breakpoint
ALTER TABLE "organization_pairing" DROP COLUMN "first_connect_deployment_evaluated_at";