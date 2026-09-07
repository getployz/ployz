CREATE TABLE "organization" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"name" text NOT NULL,
	"slug" text NOT NULL UNIQUE,
	"logo" text,
	"metadata" text,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "account" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	"account_id" text NOT NULL,
	"provider_id" text NOT NULL,
	"user_id" uuid NOT NULL,
	"access_token" text,
	"refresh_token" text,
	"id_token" text,
	"access_token_expires_at" timestamp with time zone,
	"refresh_token_expires_at" timestamp with time zone,
	"scope" text,
	"password" text
);
--> statement-breakpoint
CREATE TABLE "invitation" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"email" text NOT NULL,
	"role" text,
	"status" text DEFAULT 'pending' NOT NULL,
	"expires_at" timestamp with time zone NOT NULL,
	"inviter_id" uuid NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "member" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"user_id" uuid NOT NULL,
	"organization_id" uuid NOT NULL,
	"role" text DEFAULT 'member' NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "member_user_id_organization_id_unique" UNIQUE("user_id","organization_id")
);
--> statement-breakpoint
CREATE TABLE "session" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	"user_id" uuid NOT NULL,
	"expires_at" timestamp with time zone NOT NULL,
	"token" text NOT NULL UNIQUE,
	"active_organization_id" uuid,
	"active_organization_slug" text,
	"ip_address" text,
	"user_agent" text
);
--> statement-breakpoint
CREATE TABLE "user" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	"email" text NOT NULL UNIQUE,
	"email_verified" boolean DEFAULT false NOT NULL,
	"name" text NOT NULL,
	"image" text
);
--> statement-breakpoint
CREATE TABLE "verification" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	"identifier" text NOT NULL,
	"value" text NOT NULL,
	"expires_at" timestamp with time zone NOT NULL
);
--> statement-breakpoint
CREATE TABLE "environment" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"project_id" uuid NOT NULL,
	"organization_id" uuid NOT NULL,
	"name" text NOT NULL,
	"namespace" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "environment_project_id_name_unique" UNIQUE("project_id","name"),
	CONSTRAINT "environment_project_id_id_unique" UNIQUE("project_id","id"),
	CONSTRAINT "environment_organization_id_namespace_unique" UNIQUE("organization_id","namespace")
);
--> statement-breakpoint
CREATE TABLE "project" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"name" text NOT NULL,
	"slug" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "project_organization_id_slug_unique" UNIQUE("organization_id","slug"),
	CONSTRAINT "project_organization_id_id_unique" UNIQUE("organization_id","id")
);
--> statement-breakpoint
CREATE TABLE "user_project_preference" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"user_id" uuid NOT NULL,
	"project_id" uuid NOT NULL,
	"environment_id" uuid NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "user_project_preference_user_id_project_id_unique" UNIQUE("user_id","project_id")
);
--> statement-breakpoint
CREATE TABLE "config_key" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"project_id" uuid NOT NULL,
	"scope" text NOT NULL,
	"service_lineage_id" uuid,
	"variable_group_lineage_id" uuid,
	"canonical_name" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "config_key_project_id_id_unique" UNIQUE("project_id","id"),
	CONSTRAINT "config_key_scope_owner_check" CHECK ((
        ("scope" = 'service_lineage' and "service_lineage_id" is not null and "variable_group_lineage_id" is null) or
        ("scope" = 'variable_group_lineage' and "service_lineage_id" is null and "variable_group_lineage_id" is not null)
      ))
);
--> statement-breakpoint
CREATE TABLE "config_value" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"project_id" uuid NOT NULL,
	"config_key_id" uuid NOT NULL,
	"environment_id" uuid NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "config_value_config_key_id_environment_id_unique" UNIQUE("config_key_id","environment_id")
);
--> statement-breakpoint
CREATE TABLE "environment_canvas_node_position" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"environment_id" uuid NOT NULL,
	"resource_type" text NOT NULL,
	"resource_id" uuid NOT NULL,
	"x" integer NOT NULL,
	"y" integer NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "environment_canvas_node_position_environment_id_resource_type_resource_id_unique" UNIQUE("environment_id","resource_type","resource_id")
);
--> statement-breakpoint
CREATE TABLE "environment_resource" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"project_id" uuid NOT NULL,
	"environment_id" uuid NOT NULL,
	"lineage_id" uuid NOT NULL,
	"implementation_type" text NOT NULL,
	"variable_group_id" uuid,
	"name" text NOT NULL,
	"slug" text NOT NULL,
	"deleted_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "environment_resource_project_id_id_unique" UNIQUE("project_id","id"),
	CONSTRAINT "environment_resource_environment_id_id_unique" UNIQUE("environment_id","id"),
	CONSTRAINT "environment_resource_implementation_type_check" CHECK ("implementation_type" in ('variable_group', 'volume')),
	CONSTRAINT "environment_resource_variable_group_reference_check" CHECK (("implementation_type" != 'variable_group' or "variable_group_id" is not null)),
	CONSTRAINT "environment_resource_volume_no_variable_group_check" CHECK (("implementation_type" != 'volume' or "variable_group_id" is null))
);
--> statement-breakpoint
CREATE TABLE "environment_variable_group" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"project_id" uuid NOT NULL,
	"environment_id" uuid NOT NULL,
	"lineage_id" uuid NOT NULL,
	"name" text NOT NULL,
	"slug" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "environment_variable_group_project_id_id_unique" UNIQUE("project_id","id"),
	CONSTRAINT "environment_variable_group_environment_id_slug_unique" UNIQUE("environment_id","slug"),
	CONSTRAINT "environment_variable_group_environment_id_lineage_id_unique" UNIQUE("environment_id","lineage_id")
);
--> statement-breakpoint
CREATE TABLE "resource_lineage" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"project_id" uuid NOT NULL,
	"canonical_name" text NOT NULL,
	"canonical_slug" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "resource_lineage_project_id_canonical_slug_unique" UNIQUE("project_id","canonical_slug"),
	CONSTRAINT "resource_lineage_project_id_id_unique" UNIQUE("project_id","id")
);
--> statement-breakpoint
CREATE TABLE "service" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"project_id" uuid NOT NULL,
	"environment_id" uuid NOT NULL,
	"lineage_id" uuid NOT NULL,
	"name" text NOT NULL,
	"slug" text NOT NULL,
	"source_type" text NOT NULL,
	"source_config" jsonb NOT NULL,
	"pre_deploy_command" text,
	"start_command" text,
	"healthcheck" jsonb DEFAULT '{"type":"none"}' NOT NULL,
	"restart_policy" jsonb DEFAULT '"unless-stopped"' NOT NULL,
	"max_retries" integer DEFAULT 10 NOT NULL,
	"cron" text,
	"replicas" integer DEFAULT 1 NOT NULL,
	"cpu_limit" double precision,
	"mem_limit" double precision,
	"private_dns" text NOT NULL,
	"routes" jsonb DEFAULT '[]' NOT NULL,
	"managed_hostname" jsonb,
	"build" jsonb DEFAULT '{"builder":"auto","dockerfilePath":null,"watchPaths":[]}' NOT NULL,
	"has_registry_credential" boolean DEFAULT false NOT NULL,
	"first_deployed_at" timestamp with time zone,
	"deleted_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "service_project_id_id_unique" UNIQUE("project_id","id"),
	CONSTRAINT "service_environment_id_id_unique" UNIQUE("environment_id","id")
);
--> statement-breakpoint
CREATE TABLE "service_lineage" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"project_id" uuid NOT NULL,
	"canonical_name" text NOT NULL,
	"canonical_slug" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "service_lineage_project_id_canonical_slug_unique" UNIQUE("project_id","canonical_slug"),
	CONSTRAINT "service_lineage_project_id_id_unique" UNIQUE("project_id","id")
);
--> statement-breakpoint
CREATE TABLE "service_registry_credential" (
	"service_id" uuid PRIMARY KEY,
	"encrypted_registry_username" jsonb,
	"encrypted_registry_secret" jsonb,
	CONSTRAINT "service_registry_credential_nonempty_check" CHECK (num_nonnulls("encrypted_registry_username", "encrypted_registry_secret") > 0)
);
--> statement-breakpoint
CREATE TABLE "service_variable_group_attachment" (
	"organization_id" uuid NOT NULL,
	"service_id" uuid,
	"variable_group_id" uuid,
	"sort_order" integer DEFAULT 0 NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "service_variable_group_attachment_pkey" PRIMARY KEY("service_id","variable_group_id")
);
--> statement-breakpoint
CREATE TABLE "service_volume_attachment" (
	"organization_id" uuid NOT NULL,
	"project_id" uuid NOT NULL,
	"environment_id" uuid NOT NULL,
	"service_id" uuid,
	"volume_resource_id" uuid,
	"mount_path" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "service_volume_attachment_pkey" PRIMARY KEY("service_id","volume_resource_id"),
	CONSTRAINT "service_volume_attachment_service_id_mount_path_unique" UNIQUE("service_id","mount_path")
);
--> statement-breakpoint
CREATE TABLE "variable" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"project_id" uuid NOT NULL,
	"service_id" uuid,
	"variable_group_id" uuid,
	"config_key_id" uuid NOT NULL,
	"key" text NOT NULL,
	"description" text,
	"exported" boolean DEFAULT false NOT NULL,
	"value_kind" text NOT NULL,
	"value_parts" jsonb,
	"value_fingerprint" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "variable_service_id_key_unique" UNIQUE("service_id","key"),
	CONSTRAINT "variable_variable_group_id_key_unique" UNIQUE("variable_group_id","key"),
	CONSTRAINT "variable_owner_check" CHECK (num_nonnulls("service_id", "variable_group_id") = 1),
	CONSTRAINT "variable_value_kind_check" CHECK ((
        ("value_kind" = 'plain' and "value_parts" is not null) or
        ("value_kind" = 'sealed' and "value_parts" is null)
      ))
);
--> statement-breakpoint
CREATE TABLE "variable_group_lineage" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"project_id" uuid NOT NULL,
	"canonical_name" text NOT NULL,
	"canonical_slug" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "variable_group_lineage_project_id_canonical_slug_unique" UNIQUE("project_id","canonical_slug"),
	CONSTRAINT "variable_group_lineage_project_id_id_unique" UNIQUE("project_id","id")
);
--> statement-breakpoint
CREATE TABLE "variable_secret" (
	"variable_id" uuid PRIMARY KEY,
	"encrypted_value" jsonb NOT NULL
);
--> statement-breakpoint
CREATE TABLE "environment_deployment" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"environment_id" uuid NOT NULL,
	"trigger_origin" jsonb NOT NULL,
	"saved_state_snapshot_id" uuid NOT NULL,
	"service_action_policy" jsonb,
	"status" text DEFAULT 'queued' NOT NULL,
	"inngest_run_id" text,
	"core_deploy_id" text,
	"retry_of_deployment_id" uuid,
	"variable_producers" jsonb,
	"deploy_manifest" jsonb,
	"deploy_preview" jsonb,
	"failure_code" text,
	"failure_message" text,
	"message" text,
	"cancellation_requested_at" timestamp with time zone,
	"dispatch_requested_at" timestamp with time zone,
	"started_at" timestamp with time zone,
	"finished_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "environment_deployment_cancellation_shape_check" CHECK ((
        ("status" = 'cancelled' and "cancellation_requested_at" is not null and "finished_at" is not null)
        or ("status" <> 'cancelled' and "cancellation_requested_at" is null)
      ))
);
--> statement-breakpoint
CREATE TABLE "environment_deployment_secret" (
	"environment_deployment_id" uuid PRIMARY KEY,
	"encrypted_frozen_deploy_input" jsonb NOT NULL
);
--> statement-breakpoint
CREATE TABLE "environment_saved_state_snapshot" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"environment_id" uuid NOT NULL,
	"actor_id" uuid NOT NULL,
	"message" text,
	"intent" jsonb NOT NULL,
	"volume_deletion_authorizations" jsonb NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "environment_saved_state_snapshot_environment_id_id_unique" UNIQUE("environment_id","id")
);
--> statement-breakpoint
CREATE TABLE "core_operation_event" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"watch_id" uuid NOT NULL,
	"sequence" text NOT NULL,
	"event_type" text NOT NULL,
	"schema_version" integer DEFAULT 1 NOT NULL,
	"payload" jsonb NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "core_operation_event_watch_id_sequence_unique" UNIQUE("watch_id","sequence")
);
--> statement-breakpoint
CREATE TABLE "core_operation_watch" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"operation_id" text NOT NULL,
	"expected_kind" text NOT NULL,
	"start_sequence" text NOT NULL,
	"next_sequence" text NOT NULL,
	"cursor_state" text NOT NULL,
	"observation_state" text DEFAULT 'active' NOT NULL,
	"observation_detail" jsonb,
	"inngest_run_id" text,
	"deadline_at" timestamp with time zone NOT NULL,
	"terminal_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "core_operation_watch_organization_id_operation_id_unique" UNIQUE("organization_id","operation_id"),
	CONSTRAINT "core_operation_watch_expected_kind_check" CHECK ("expected_kind" in ('deploy','cert','machine_add','machine_update','machine_lifecycle','core_replace','credential_grant','network_repair','service_restart','managed_dns_reconcile','ingress_configure','ingress_refresh','namespace_remove','volume_create','volume_remove')),
	CONSTRAINT "core_operation_watch_cursor_state_check" CHECK ("cursor_state" in ('more','caught_up','terminal')),
	CONSTRAINT "core_operation_watch_observation_state_check" CHECK ("observation_state" in ('active','core_terminal','cloud_timeout','cloud_cancelled'))
);
--> statement-breakpoint
CREATE TABLE "destructive_volume_attempt" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"environment_deployment_id" uuid NOT NULL,
	"environment_resource_id" uuid NOT NULL,
	"retry_of_attempt_id" uuid,
	"target" jsonb NOT NULL,
	"evidence" jsonb NOT NULL,
	"evidence_fingerprint" text NOT NULL,
	"disposition" text DEFAULT 'active' NOT NULL,
	"operation_id" text,
	"start_sequence" text,
	"inngest_run_id" text,
	"request_published_at" timestamp with time zone,
	"accepted_at" timestamp with time zone,
	"terminal_event" jsonb,
	"failure" jsonb,
	"deadline_at" timestamp with time zone,
	"terminal_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "destructive_volume_attempt_disposition_check" CHECK ("disposition" in ('active', 'accepted', 'completed', 'partial', 'core_terminal', 'cloud_timeout', 'cloud_cancelled', 'failed')),
	CONSTRAINT "destructive_volume_attempt_identity_check" CHECK ("target"->>'resourceId' = "environment_resource_id"::text
        and "evidence"->>'fingerprint' = "evidence_fingerprint"),
	CONSTRAINT "destructive_volume_attempt_evidence_check" CHECK ((
        "disposition" = 'active'
        and "operation_id" is null
        and "start_sequence" is null
        and "accepted_at" is null
        and "terminal_at" is null
      ) or (
        "disposition" = 'accepted'
        and "operation_id" is not null
        and "start_sequence" is not null
        and "accepted_at" is not null
        and "terminal_at" is null
      ) or (
        "disposition" in ('completed', 'partial', 'core_terminal', 'cloud_timeout')
        and "operation_id" is not null
        and "start_sequence" is not null
        and "accepted_at" is not null
        and "terminal_at" is not null
      ) or (
        "disposition" = 'cloud_cancelled'
        and (
          ("operation_id" is null and "start_sequence" is null and "accepted_at" is null)
          or ("operation_id" is not null and "start_sequence" is not null and "accepted_at" is not null)
        )
        and "terminal_at" is not null
      ) or (
        "disposition" = 'failed'
        and "operation_id" is null
        and "start_sequence" is null
        and "accepted_at" is null
        and "terminal_at" is not null
      )),
	CONSTRAINT "destructive_volume_attempt_terminal_payload_check" CHECK ((
        "disposition" in ('active', 'accepted', 'completed')
        and "failure" is null
      ) or (
        "disposition" in ('partial', 'failed')
        and "failure" is not null
      ) or "disposition" in ('core_terminal', 'cloud_timeout', 'cloud_cancelled')),
	CONSTRAINT "destructive_volume_attempt_workflow_check" CHECK (("inngest_run_id" is null) = ("deadline_at" is null))
);
--> statement-breakpoint
CREATE TABLE "environment_node_config_snapshot" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"environment_deployment_id" uuid NOT NULL,
	"environment_id" uuid NOT NULL,
	"node_type" text NOT NULL,
	"node_id" uuid NOT NULL,
	"node_lineage_id" uuid NOT NULL,
	"config_version" integer DEFAULT 1 NOT NULL,
	"config" jsonb NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "environment_node_config_snapshot_environment_deployment_id_node_type_node_id_unique" UNIQUE("environment_deployment_id","node_type","node_id"),
	CONSTRAINT "environment_node_config_snapshot_node_type_check" CHECK ("node_type" in ('service', 'variable_group', 'volume'))
);
--> statement-breakpoint
CREATE TABLE "environment_node_config_snapshot_secret" (
	"snapshot_id" uuid PRIMARY KEY,
	"encrypted_registry_username" jsonb,
	"encrypted_registry_secret" jsonb,
	CONSTRAINT "environment_node_config_snapshot_secret_nonempty_check" CHECK (num_nonnulls("encrypted_registry_username", "encrypted_registry_secret") > 0)
);
--> statement-breakpoint
CREATE TABLE "environment_node_introduction" (
	"organization_id" uuid NOT NULL,
	"environment_id" uuid,
	"node_type" text,
	"node_id" uuid,
	"node_lineage_id" uuid NOT NULL,
	"config_version" integer DEFAULT 1 NOT NULL,
	"config" jsonb NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "environment_node_introduction_pkey" PRIMARY KEY("environment_id","node_type","node_id"),
	CONSTRAINT "environment_node_introduction_node_type_check" CHECK ("node_type" in ('service', 'variable_group', 'volume'))
);
--> statement-breakpoint
CREATE TABLE "environment_node_introduction_secret" (
	"environment_id" uuid,
	"node_type" text,
	"node_id" uuid,
	"encrypted_registry_username" jsonb,
	"encrypted_registry_secret" jsonb,
	CONSTRAINT "environment_node_introduction_secret_pkey" PRIMARY KEY("environment_id","node_type","node_id"),
	CONSTRAINT "environment_node_introduction_secret_nonempty_check" CHECK (num_nonnulls("encrypted_registry_username", "encrypted_registry_secret") > 0)
);
--> statement-breakpoint
CREATE TABLE "organization_pairing" (
	"organization_id" uuid PRIMARY KEY,
	"encrypted_pairing_secret" jsonb NOT NULL,
	"founder_public_key" text,
	"founder_machine_id" text,
	"first_connect_deployment_evaluated_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "organization_pairing_state_check" CHECK ("founder_public_key" is not null or "founder_machine_id" is not null),
	CONSTRAINT "organization_pairing_founder_machine_id_check" CHECK ("founder_machine_id" is null or "founder_machine_id" ~ '^[0-9a-f]{32}$')
);
--> statement-breakpoint
CREATE TABLE "teardown_attempt" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"requested_by_user_id" uuid NOT NULL,
	"project_id" uuid,
	"environment_id" uuid,
	"retry_of_attempt_id" uuid,
	"scope" text NOT NULL,
	"confirm_data_loss" jsonb NOT NULL,
	"targets" jsonb NOT NULL,
	"status" text DEFAULT 'pending' NOT NULL,
	"inngest_run_id" text,
	"outcome" jsonb,
	"failure_message" text,
	"started_at" timestamp with time zone,
	"terminal_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "teardown_attempt_scope_check" CHECK ("scope" in ('environment','project','organization')),
	CONSTRAINT "teardown_attempt_status_check" CHECK ("status" in ('pending','running','completed','partial','failed','cancelled')),
	CONSTRAINT "teardown_attempt_scope_ids_check" CHECK ((
        ("scope" = 'environment' and "environment_id" is not null
          and "project_id" is not null)
        or ("scope" = 'project' and "project_id" is not null
          and "environment_id" is null)
        or ("scope" = 'organization' and "project_id" is null
          and "environment_id" is null)
      )),
	CONSTRAINT "teardown_attempt_confirm_data_loss_check" CHECK (jsonb_typeof("confirm_data_loss") = 'array'),
	CONSTRAINT "teardown_attempt_targets_check" CHECK (jsonb_typeof("targets") = 'object'),
	CONSTRAINT "teardown_attempt_status_shape_check" CHECK ((
        ("status" = 'pending' and "inngest_run_id" is null
          and "started_at" is null and "terminal_at" is null
          and "outcome" is null and "failure_message" is null)
        or ("status" = 'running' and "inngest_run_id" is not null
          and length("inngest_run_id") between 1 and 255
          and "started_at" is not null and "terminal_at" is null
          and "outcome" is null and "failure_message" is null)
        or ("status" = 'completed' and "inngest_run_id" is not null
          and "started_at" is not null and "terminal_at" is not null
          and "outcome" is not null and "failure_message" is null)
        or ("status" = 'partial' and "inngest_run_id" is not null
          and "started_at" is not null and "terminal_at" is not null
          and "outcome" is not null)
        or ("status" in ('failed', 'cancelled')
          and "inngest_run_id" is not null
          and "started_at" is not null and "terminal_at" is not null
          and "failure_message" is not null
          and length("failure_message") between 1 and 2000)
      ))
);
--> statement-breakpoint
CREATE TABLE "volume_remove_attempt" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"requested_by_user_id" uuid NOT NULL,
	"environment_id" uuid NOT NULL,
	"environment_resource_id" uuid,
	"retry_of_attempt_id" uuid,
	"volumes" jsonb NOT NULL,
	"status" text DEFAULT 'pending' NOT NULL,
	"inngest_run_id" text,
	"outcome" jsonb,
	"failure_message" text,
	"started_at" timestamp with time zone,
	"terminal_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "volume_remove_attempt_status_check" CHECK ("status" in ('pending','running','completed','partial','failed','cancelled')),
	CONSTRAINT "volume_remove_attempt_volumes_check" CHECK (jsonb_typeof("volumes") = 'array'
        and jsonb_array_length("volumes") >= 1),
	CONSTRAINT "volume_remove_attempt_status_shape_check" CHECK ((
        ("status" = 'pending' and "inngest_run_id" is null
          and "started_at" is null and "terminal_at" is null
          and "outcome" is null and "failure_message" is null)
        or ("status" = 'running' and "inngest_run_id" is not null
          and length("inngest_run_id") between 1 and 255
          and "started_at" is not null and "terminal_at" is null
          and "outcome" is null and "failure_message" is null)
        or ("status" = 'completed' and "inngest_run_id" is not null
          and "started_at" is not null and "terminal_at" is not null
          and "outcome" is not null and "failure_message" is null)
        or ("status" = 'partial' and "inngest_run_id" is not null
          and "started_at" is not null and "terminal_at" is not null
          and "outcome" is not null)
        or ("status" in ('failed', 'cancelled')
          and "inngest_run_id" is not null
          and "started_at" is not null and "terminal_at" is not null
          and "failure_message" is not null
          and length("failure_message") between 1 and 2000)
      ))
);
--> statement-breakpoint
CREATE TABLE "machine_enrollment_token" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"token_hash" text NOT NULL,
	"created_by_user_id" uuid NOT NULL,
	"expires_at" timestamp with time zone NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "machine_enrollment_token_hash_check" CHECK ("token_hash" ~ '^[0-9a-f]{64}$')
);
--> statement-breakpoint
CREATE TABLE "machine_remove_attempt" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"requested_by_user_id" uuid NOT NULL,
	"machine_id" text NOT NULL,
	"confirm_data_loss" jsonb NOT NULL,
	"state" text DEFAULT 'pending' NOT NULL,
	"inngest_run_id" text,
	"missing_identities" jsonb,
	"failure_code" text,
	"failure_message" text,
	"started_at" timestamp with time zone,
	"terminal_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "machine_remove_attempt_machine_id_check" CHECK (length("machine_id") between 1 and 64 and "machine_id" !~ '[[:cntrl:]]'),
	CONSTRAINT "machine_remove_attempt_confirm_data_loss_check" CHECK (jsonb_typeof("confirm_data_loss") = 'array'),
	CONSTRAINT "machine_remove_attempt_state_check" CHECK ("state" in ('pending','running','succeeded','failed','cancelled','missing_identities')),
	CONSTRAINT "machine_remove_attempt_state_shape_check" CHECK ((
        ("state" = 'pending' and "inngest_run_id" is null
          and "started_at" is null and "terminal_at" is null
          and "failure_code" is null and "failure_message" is null
          and "missing_identities" is null)
        or ("state" = 'running' and "inngest_run_id" is not null
          and length("inngest_run_id") between 1 and 255
          and "started_at" is not null and "terminal_at" is null
          and "failure_code" is null and "failure_message" is null
          and "missing_identities" is null)
        or ("state" = 'succeeded' and "inngest_run_id" is not null
          and length("inngest_run_id") between 1 and 255
          and "started_at" is not null and "terminal_at" is not null
          and "failure_code" is null and "failure_message" is null
          and "missing_identities" is null)
        or ("state" in ('failed','cancelled')
          and "inngest_run_id" is not null
          and length("inngest_run_id") between 1 and 255
          and "started_at" is not null and "terminal_at" is not null
          and "failure_code" is not null and "failure_message" is not null
          and "failure_code" ~ '^[a-z][a-z0-9_]{0,63}$'
          and length("failure_message") between 1 and 1024
          and "missing_identities" is null)
        or ("state" = 'missing_identities'
          and "inngest_run_id" is not null
          and length("inngest_run_id") between 1 and 255
          and "started_at" is not null and "terminal_at" is not null
          and "failure_code" is null and "failure_message" is null
          and jsonb_typeof("missing_identities") = 'array'
          and jsonb_array_length("missing_identities") > 0)
      ))
);
--> statement-breakpoint
CREATE TABLE "organization_machine" (
	"organization_id" uuid,
	"machine_id" text,
	"is_dial_entry" boolean DEFAULT false NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "organization_machine_pkey" PRIMARY KEY("organization_id","machine_id"),
	CONSTRAINT "organization_machine_id_format_check" CHECK ("machine_id" ~ '^[0-9a-f]{32}$')
);
--> statement-breakpoint
CREATE TABLE "github_branch_projection" (
	"installation_id" integer,
	"repository_id" bigint,
	"ref" text,
	"state" text NOT NULL,
	"evaluated_head_sha" text,
	"evaluation_reason" text NOT NULL,
	"last_delivery_id" text NOT NULL,
	"last_receipt_sequence" bigint NOT NULL,
	"evaluation_revision" bigint DEFAULT 1 NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "github_branch_projection_pkey" PRIMARY KEY("installation_id","repository_id","ref"),
	CONSTRAINT "github_branch_projection_identity_check" CHECK ("installation_id" > 0 and "repository_id" > 0 and "last_receipt_sequence" > 0 and "evaluation_revision" > 0),
	CONSTRAINT "github_branch_projection_ref_check" CHECK ("ref" like 'refs/heads/%' and length("ref") > length('refs/heads/')),
	CONSTRAINT "github_branch_projection_state_check" CHECK ("state" in ('active','deleted')),
	CONSTRAINT "github_branch_projection_evaluation_reason_check" CHECK ("evaluation_reason" in ('first_observation','changed_paths','rebaseline_all_services','branch_deleted')),
	CONSTRAINT "github_branch_projection_head_check" CHECK ((
        ("state" = 'active' and "evaluated_head_sha" ~ '^[0-9a-f]{40}$' and "evaluation_reason" != 'branch_deleted')
        or ("state" = 'deleted' and "evaluated_head_sha" is null and "evaluation_reason" = 'branch_deleted')
      ))
);
--> statement-breakpoint
CREATE TABLE "github_check_suite_projection" (
	"installation_id" integer,
	"repository_id" bigint,
	"check_suite_id" bigint,
	"head_sha" text NOT NULL,
	"status" text NOT NULL,
	"conclusion" text,
	"source_updated_at" timestamp with time zone NOT NULL,
	"last_delivery_id" text NOT NULL,
	"last_receipt_sequence" bigint NOT NULL,
	"transition_revision" integer DEFAULT 1 NOT NULL,
	"published_revision" integer DEFAULT 0 NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "github_check_suite_projection_pkey" PRIMARY KEY("installation_id","repository_id","check_suite_id"),
	CONSTRAINT "github_check_suite_projection_identity_check" CHECK ("installation_id" > 0 and "repository_id" > 0 and "check_suite_id" > 0 and "last_receipt_sequence" > 0),
	CONSTRAINT "github_check_suite_projection_head_sha_check" CHECK ("head_sha" ~ '^[0-9a-f]{40}$'),
	CONSTRAINT "github_check_suite_projection_status_check" CHECK ("status" in ('queued','in_progress','completed','pending','waiting','requested')),
	CONSTRAINT "github_check_suite_projection_conclusion_check" CHECK ("conclusion" is null or "conclusion" in ('action_required','cancelled','failure','neutral','success','skipped','stale','timed_out','startup_failure')),
	CONSTRAINT "github_check_suite_projection_revision_check" CHECK ("transition_revision" >= 1 and "published_revision" >= 0 and "published_revision" <= "transition_revision")
);
--> statement-breakpoint
CREATE TABLE "github_environment_trigger" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"installation_id" integer NOT NULL,
	"repository_id" bigint NOT NULL,
	"ref" text NOT NULL,
	"head_sha" text NOT NULL,
	"environment_id" uuid NOT NULL,
	"service_ids" text[] NOT NULL,
	"selection_mode" text NOT NULL,
	"reason" text NOT NULL,
	"source_delivery_id" text NOT NULL,
	"source_receipt_sequence" bigint NOT NULL,
	"branch_evaluation_revision" bigint DEFAULT 1 NOT NULL,
	"trigger_revision" bigint DEFAULT 1 NOT NULL,
	"published_revision" bigint DEFAULT 0 NOT NULL,
	"publish_state" text DEFAULT 'pending' NOT NULL,
	"published_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "github_environment_trigger_installation_id_repository_id_ref_branch_evaluation_revision_environment_id_unique" UNIQUE("installation_id","repository_id","ref","branch_evaluation_revision","environment_id"),
	CONSTRAINT "github_environment_trigger_identity_check" CHECK ("installation_id" > 0 and "repository_id" > 0 and "source_receipt_sequence" > 0 and "branch_evaluation_revision" > 0),
	CONSTRAINT "github_environment_trigger_ref_check" CHECK ("ref" like 'refs/heads/%' and length("ref") > length('refs/heads/')),
	CONSTRAINT "github_environment_trigger_head_sha_check" CHECK ("head_sha" ~ '^[0-9a-f]{40}$'),
	CONSTRAINT "github_environment_trigger_service_ids_check" CHECK (cardinality("service_ids") > 0 and array_position("service_ids", null) is null),
	CONSTRAINT "github_environment_trigger_selection_mode_check" CHECK ("selection_mode" in ('paths','all_services')),
	CONSTRAINT "github_environment_trigger_reason_check" CHECK ("reason" in ('first_observation','changed_paths','force_rebaseline','non_ancestor_rebaseline','changed_paths_incomplete_rebaseline')),
	CONSTRAINT "github_environment_trigger_selection_reason_check" CHECK ((
        ("selection_mode" = 'paths' and "reason" = 'changed_paths')
        or ("selection_mode" = 'all_services' and "reason" in ('first_observation','force_rebaseline','non_ancestor_rebaseline','changed_paths_incomplete_rebaseline'))
      )),
	CONSTRAINT "github_environment_trigger_publish_revision_check" CHECK ("trigger_revision" = "branch_evaluation_revision" and "published_revision" >= 0 and "published_revision" <= "trigger_revision"),
	CONSTRAINT "github_environment_trigger_publish_state_check" CHECK ((
        ("publish_state" = 'pending' and "published_revision" < "trigger_revision" and "published_at" is null)
        or ("publish_state" = 'published' and "published_revision" = "trigger_revision" and "published_at" is not null)
      ))
);
--> statement-breakpoint
CREATE TABLE "github_installation" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"user_id" uuid NOT NULL,
	"installation_id" integer NOT NULL,
	"account_login" text NOT NULL,
	"account_type" text NOT NULL,
	"account_avatar_url" text,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "github_installation_user_id_installation_id_unique" UNIQUE("user_id","installation_id")
);
--> statement-breakpoint
CREATE TABLE "github_repository_cache" (
	"user_id" uuid,
	"installation_id" integer,
	"repository_id" bigint,
	"name" text NOT NULL,
	"full_name" text NOT NULL,
	"default_branch" text NOT NULL,
	"private" boolean NOT NULL,
	"html_url" text NOT NULL,
	"repo_updated_at" timestamp with time zone NOT NULL,
	"synced_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "github_repository_cache_pkey" PRIMARY KEY("user_id","installation_id","repository_id")
);
--> statement-breakpoint
CREATE TABLE "github_webhook_delivery" (
	"delivery_id" text PRIMARY KEY,
	"receipt_sequence" bigserial UNIQUE,
	"event_kind" text NOT NULL,
	"processing_state" text DEFAULT 'received' NOT NULL,
	"outcome" text,
	"failure_code" text,
	"installation_id" integer,
	"repository_id" bigint,
	"ref" text,
	"branch_state" text,
	"head_sha" text,
	"check_suite_id" bigint,
	"check_suite_action" text,
	"check_suite_status" text,
	"check_suite_conclusion" text,
	"processing_run_id" text,
	"processing_started_at" timestamp with time zone,
	"processed_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "github_webhook_delivery_delivery_id_receipt_sequence_unique" UNIQUE("delivery_id","receipt_sequence"),
	CONSTRAINT "github_webhook_delivery_identity_check" CHECK (length(trim("delivery_id")) > 0 and "receipt_sequence" > 0 and ("installation_id" is null or "installation_id" > 0) and ("repository_id" is null or "repository_id" > 0) and ("check_suite_id" is null or "check_suite_id" > 0)),
	CONSTRAINT "github_webhook_delivery_event_kind_check" CHECK ("event_kind" in ('push','check_suite')),
	CONSTRAINT "github_webhook_delivery_processing_state_check" CHECK ("processing_state" in ('received','processing','processed','rejected','failed','cancelled')),
	CONSTRAINT "github_webhook_delivery_outcome_check" CHECK ("outcome" is null or "outcome" in ('branch_projected','branch_deleted','branch_rebased_all_services','check_suite_projected','check_suite_unchanged','ignored_stale','ignored_unconfigured_repository','ignored_no_matching_service','malformed','identity_unresolved','unsupported_action','processing_failed','cancelled')),
	CONSTRAINT "github_webhook_delivery_branch_state_check" CHECK ("branch_state" is null or "branch_state" in ('active','deleted')),
	CONSTRAINT "github_webhook_delivery_failure_code_check" CHECK ("failure_code" is null or "failure_code" in ('malformed_payload','identity_unresolved','unsupported_action','observation_failed','persistence_failed','publication_failed','retry_exhausted','unexpected_error','inngest_cancelled')),
	CONSTRAINT "github_webhook_delivery_check_suite_action_check" CHECK ("check_suite_action" is null or "check_suite_action" in ('requested','rerequested','completed')),
	CONSTRAINT "github_webhook_delivery_check_suite_status_check" CHECK ("check_suite_status" is null or "check_suite_status" in ('queued','in_progress','completed','pending','waiting','requested')),
	CONSTRAINT "github_webhook_delivery_check_suite_conclusion_check" CHECK ("check_suite_conclusion" is null or "check_suite_conclusion" in ('action_required','cancelled','failure','neutral','success','skipped','stale','timed_out','startup_failure')),
	CONSTRAINT "github_webhook_delivery_head_sha_check" CHECK ("head_sha" is null or "head_sha" ~ '^[0-9a-f]{40}$'),
	CONSTRAINT "github_webhook_delivery_processing_evidence_check" CHECK ((
        ("processing_state" = 'received' and "processing_run_id" is null and "processing_started_at" is null and "outcome" is null and "failure_code" is null and "processed_at" is null)
        or
        ("processing_state" = 'processing' and "processing_run_id" is not null and "processing_started_at" is not null and "outcome" is null and "failure_code" is null and "processed_at" is null)
        or
        ("processing_state" = 'processed' and "processing_run_id" is not null and "processing_started_at" is not null and "outcome" in ('branch_projected','branch_deleted','branch_rebased_all_services','check_suite_projected','check_suite_unchanged','ignored_stale','ignored_unconfigured_repository','ignored_no_matching_service') and "failure_code" is null and "processed_at" is not null)
        or
        ("processing_state" = 'rejected' and (
          ("outcome" = 'malformed' and "failure_code" = 'malformed_payload')
          or ("outcome" = 'identity_unresolved' and "failure_code" = 'identity_unresolved')
          or ("outcome" = 'unsupported_action' and "failure_code" = 'unsupported_action')
        ) and "processing_run_id" is null and "processing_started_at" is null and "processed_at" is not null)
        or
        ("processing_state" = 'failed' and "processing_run_id" is not null and "processing_started_at" is not null and "outcome" = 'processing_failed' and "failure_code" in ('observation_failed','persistence_failed','publication_failed','retry_exhausted','unexpected_error') and "processed_at" is not null)
        or
        ("processing_state" = 'cancelled' and "processing_run_id" is not null and "processing_started_at" is not null and "outcome" = 'cancelled' and "failure_code" = 'inngest_cancelled' and "processed_at" is not null)
      )),
	CONSTRAINT "github_webhook_delivery_processed_summary_check" CHECK ((
        "processing_state" not in ('processing','processed','failed','cancelled')
        or (
          "installation_id" is not null
          and "repository_id" is not null
          and (
            (
              "event_kind" = 'push'
              and "ref" is not null
              and "ref" like 'refs/heads/%'
              and "branch_state" is not null
              and "check_suite_id" is null
              and "check_suite_action" is null
              and "check_suite_status" is null
              and "check_suite_conclusion" is null
              and (
                ("branch_state" = 'active' and "head_sha" is not null)
                or ("branch_state" = 'deleted' and "head_sha" is null)
              )
            )
            or (
              "event_kind" = 'check_suite'
              and "branch_state" is null
              and "head_sha" is not null
              and "check_suite_id" is not null
              and "check_suite_action" is not null
              and "check_suite_status" is not null
            )
          )
        )
      )),
	CONSTRAINT "github_webhook_delivery_outcome_kind_check" CHECK ((
        "outcome" not in ('branch_projected','branch_deleted','branch_rebased_all_services','check_suite_projected','check_suite_unchanged')
        or ("outcome" in ('branch_projected','branch_deleted','branch_rebased_all_services') and "event_kind" = 'push')
        or ("outcome" in ('check_suite_projected','check_suite_unchanged') and "event_kind" = 'check_suite')
      ))
);
--> statement-breakpoint
CREATE TABLE "organization_billing_state" (
	"organization_id" uuid PRIMARY KEY,
	"active_subscription_id" text,
	"current_plan" text,
	"product_id" uuid,
	"amount" integer,
	"currency" text,
	"current_period_start" timestamp with time zone,
	"current_period_end" timestamp with time zone,
	"has_active_subscription" boolean DEFAULT false NOT NULL,
	"synced_at" timestamp with time zone DEFAULT now() NOT NULL,
	"source_updated_at" timestamp with time zone,
	CONSTRAINT "organization_billing_state_active_fields_check" CHECK ((
        "has_active_subscription" = false
        OR (
          "active_subscription_id" IS NOT NULL
          AND "current_plan" IS NOT NULL
          AND "product_id" IS NOT NULL
          AND "amount" IS NOT NULL
          AND "currency" IS NOT NULL
          AND "current_period_start" IS NOT NULL
          AND "current_period_end" IS NOT NULL
        )
      )),
	CONSTRAINT "organization_billing_state_current_plan_check" CHECK ("current_plan" IS NULL OR "current_plan" IN ('free', 'solo', 'teams', 'hobby', 'pro'))
);
--> statement-breakpoint
CREATE TABLE "waitlist" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"email" text NOT NULL UNIQUE,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE INDEX "account_user_id_idx" ON "account" ("user_id");--> statement-breakpoint
CREATE INDEX "account_provider_account_idx" ON "account" ("provider_id","account_id");--> statement-breakpoint
CREATE INDEX "invitation_organization_id_idx" ON "invitation" ("organization_id");--> statement-breakpoint
CREATE INDEX "invitation_email_idx" ON "invitation" ("email");--> statement-breakpoint
CREATE INDEX "session_user_id_idx" ON "session" ("user_id");--> statement-breakpoint
CREATE INDEX "verification_identifier_idx" ON "verification" ("identifier");--> statement-breakpoint
CREATE INDEX "user_project_preference_user_idx" ON "user_project_preference" ("user_id");--> statement-breakpoint
CREATE INDEX "user_project_preference_organization_idx" ON "user_project_preference" ("organization_id");--> statement-breakpoint
CREATE INDEX "user_project_preference_project_idx" ON "user_project_preference" ("project_id");--> statement-breakpoint
CREATE UNIQUE INDEX "config_key_service_lineage_canonical_name_unique" ON "config_key" ("project_id","service_lineage_id","canonical_name") WHERE "scope" = 'service_lineage';--> statement-breakpoint
CREATE UNIQUE INDEX "config_key_variable_group_lineage_canonical_name_unique" ON "config_key" ("project_id","variable_group_lineage_id","canonical_name") WHERE "scope" = 'variable_group_lineage';--> statement-breakpoint
CREATE INDEX "config_key_project_id_idx" ON "config_key" ("project_id");--> statement-breakpoint
CREATE INDEX "config_key_service_lineage_id_idx" ON "config_key" ("service_lineage_id");--> statement-breakpoint
CREATE INDEX "config_key_variable_group_lineage_id_idx" ON "config_key" ("variable_group_lineage_id");--> statement-breakpoint
CREATE INDEX "config_value_project_id_idx" ON "config_value" ("project_id");--> statement-breakpoint
CREATE INDEX "config_value_environment_id_idx" ON "config_value" ("environment_id");--> statement-breakpoint
CREATE INDEX "environment_canvas_node_position_environment_id_idx" ON "environment_canvas_node_position" ("environment_id");--> statement-breakpoint
CREATE INDEX "environment_canvas_node_position_organization_id_idx" ON "environment_canvas_node_position" ("organization_id");--> statement-breakpoint
CREATE INDEX "environment_canvas_node_position_resource_lookup_idx" ON "environment_canvas_node_position" ("resource_type","resource_id");--> statement-breakpoint
CREATE UNIQUE INDEX "environment_resource_environment_slug_unique" ON "environment_resource" ("environment_id","slug") WHERE "deleted_at" is null;--> statement-breakpoint
CREATE UNIQUE INDEX "environment_resource_environment_lineage_unique" ON "environment_resource" ("environment_id","lineage_id") WHERE "deleted_at" is null;--> statement-breakpoint
CREATE UNIQUE INDEX "environment_resource_variable_group_variable_group_unique" ON "environment_resource" ("variable_group_id") WHERE "implementation_type" = 'variable_group';--> statement-breakpoint
CREATE INDEX "environment_resource_project_id_idx" ON "environment_resource" ("project_id");--> statement-breakpoint
CREATE INDEX "environment_resource_organization_id_idx" ON "environment_resource" ("organization_id");--> statement-breakpoint
CREATE INDEX "environment_resource_environment_id_idx" ON "environment_resource" ("environment_id");--> statement-breakpoint
CREATE INDEX "environment_resource_lineage_id_idx" ON "environment_resource" ("lineage_id");--> statement-breakpoint
CREATE INDEX "environment_resource_variable_group_id_idx" ON "environment_resource" ("variable_group_id");--> statement-breakpoint
CREATE INDEX "environment_variable_group_project_id_idx" ON "environment_variable_group" ("project_id");--> statement-breakpoint
CREATE INDEX "environment_variable_group_organization_id_idx" ON "environment_variable_group" ("organization_id");--> statement-breakpoint
CREATE INDEX "environment_variable_group_environment_id_idx" ON "environment_variable_group" ("environment_id");--> statement-breakpoint
CREATE INDEX "environment_variable_group_lineage_id_idx" ON "environment_variable_group" ("lineage_id");--> statement-breakpoint
CREATE INDEX "resource_lineage_project_id_idx" ON "resource_lineage" ("project_id");--> statement-breakpoint
CREATE INDEX "resource_lineage_organization_id_idx" ON "resource_lineage" ("organization_id");--> statement-breakpoint
CREATE UNIQUE INDEX "service_environment_slug_unique" ON "service" ("environment_id","slug") WHERE "deleted_at" is null;--> statement-breakpoint
CREATE UNIQUE INDEX "service_environment_lineage_unique" ON "service" ("environment_id","lineage_id") WHERE "deleted_at" is null;--> statement-breakpoint
CREATE INDEX "service_project_id_idx" ON "service" ("project_id");--> statement-breakpoint
CREATE INDEX "service_organization_id_idx" ON "service" ("organization_id");--> statement-breakpoint
CREATE INDEX "service_environment_id_idx" ON "service" ("environment_id");--> statement-breakpoint
CREATE INDEX "service_lineage_id_idx" ON "service" ("lineage_id");--> statement-breakpoint
CREATE INDEX "service_lineage_project_id_idx" ON "service_lineage" ("project_id");--> statement-breakpoint
CREATE INDEX "service_variable_group_attachment_service_id_idx" ON "service_variable_group_attachment" ("service_id");--> statement-breakpoint
CREATE INDEX "service_variable_group_attachment_organization_id_idx" ON "service_variable_group_attachment" ("organization_id");--> statement-breakpoint
CREATE INDEX "service_variable_group_attachment_set_id_idx" ON "service_variable_group_attachment" ("variable_group_id");--> statement-breakpoint
CREATE INDEX "service_volume_attachment_service_id_idx" ON "service_volume_attachment" ("service_id");--> statement-breakpoint
CREATE INDEX "service_volume_attachment_volume_resource_id_idx" ON "service_volume_attachment" ("volume_resource_id");--> statement-breakpoint
CREATE INDEX "service_volume_attachment_project_id_idx" ON "service_volume_attachment" ("project_id");--> statement-breakpoint
CREATE INDEX "service_volume_attachment_organization_id_idx" ON "service_volume_attachment" ("organization_id");--> statement-breakpoint
CREATE INDEX "service_volume_attachment_environment_id_idx" ON "service_volume_attachment" ("environment_id");--> statement-breakpoint
CREATE INDEX "variable_project_id_idx" ON "variable" ("project_id");--> statement-breakpoint
CREATE INDEX "variable_organization_id_idx" ON "variable" ("organization_id");--> statement-breakpoint
CREATE INDEX "variable_service_id_idx" ON "variable" ("service_id");--> statement-breakpoint
CREATE INDEX "variable_variable_group_id_idx" ON "variable" ("variable_group_id");--> statement-breakpoint
CREATE INDEX "variable_config_key_id_idx" ON "variable" ("config_key_id");--> statement-breakpoint
CREATE INDEX "variable_exported_idx" ON "variable" ("exported");--> statement-breakpoint
CREATE UNIQUE INDEX "variable_service_config_key_unique" ON "variable" ("service_id","config_key_id") WHERE "service_id" is not null;--> statement-breakpoint
CREATE UNIQUE INDEX "variable_group_config_key_unique" ON "variable" ("variable_group_id","config_key_id") WHERE "variable_group_id" is not null;--> statement-breakpoint
CREATE INDEX "variable_group_lineage_project_id_idx" ON "variable_group_lineage" ("project_id");--> statement-breakpoint
CREATE INDEX "environment_deployment_organization_id_idx" ON "environment_deployment" ("organization_id");--> statement-breakpoint
CREATE INDEX "environment_deployment_environment_id_idx" ON "environment_deployment" ("environment_id");--> statement-breakpoint
CREATE INDEX "environment_deployment_saved_state_snapshot_id_idx" ON "environment_deployment" ("saved_state_snapshot_id");--> statement-breakpoint
CREATE UNIQUE INDEX "environment_deployment_one_queued_target_idx" ON "environment_deployment" ("environment_id") WHERE "status" = 'queued';--> statement-breakpoint
CREATE UNIQUE INDEX "environment_deployment_one_started_attempt_idx" ON "environment_deployment" ("environment_id") WHERE "status" in ('planning','deploying');--> statement-breakpoint
CREATE INDEX "environment_deployment_inngest_run_id_idx" ON "environment_deployment" ("inngest_run_id");--> statement-breakpoint
CREATE INDEX "environment_deployment_retry_of_idx" ON "environment_deployment" ("retry_of_deployment_id");--> statement-breakpoint
CREATE INDEX "environment_saved_state_snapshot_organization_id_idx" ON "environment_saved_state_snapshot" ("organization_id");--> statement-breakpoint
CREATE INDEX "environment_saved_state_snapshot_environment_created_at_idx" ON "environment_saved_state_snapshot" ("environment_id","created_at");--> statement-breakpoint
CREATE INDEX "destructive_volume_attempt_organization_id_idx" ON "destructive_volume_attempt" ("organization_id");--> statement-breakpoint
CREATE INDEX "destructive_volume_attempt_deployment_idx" ON "destructive_volume_attempt" ("environment_deployment_id","created_at");--> statement-breakpoint
CREATE INDEX "destructive_volume_attempt_resource_idx" ON "destructive_volume_attempt" ("environment_resource_id","created_at");--> statement-breakpoint
CREATE UNIQUE INDEX "destructive_volume_attempt_retry_of_idx" ON "destructive_volume_attempt" ("retry_of_attempt_id") WHERE "retry_of_attempt_id" is not null;--> statement-breakpoint
CREATE UNIQUE INDEX "destructive_volume_attempt_one_active_target_idx" ON "destructive_volume_attempt" ("environment_resource_id") WHERE "disposition" in ('active', 'accepted');--> statement-breakpoint
CREATE INDEX "destructive_volume_attempt_operation_id_idx" ON "destructive_volume_attempt" ("operation_id") WHERE "operation_id" is not null;--> statement-breakpoint
CREATE UNIQUE INDEX "destructive_volume_attempt_inngest_run_id_idx" ON "destructive_volume_attempt" ("inngest_run_id") WHERE "inngest_run_id" is not null;--> statement-breakpoint
CREATE INDEX "destructive_volume_attempt_unpublished_request_idx" ON "destructive_volume_attempt" ("created_at","id") WHERE "request_published_at" is null;--> statement-breakpoint
CREATE INDEX "environment_node_config_snapshot_organization_id_idx" ON "environment_node_config_snapshot" ("organization_id");--> statement-breakpoint
CREATE INDEX "environment_node_config_snapshot_environment_id_idx" ON "environment_node_config_snapshot" ("environment_id");--> statement-breakpoint
CREATE INDEX "environment_node_config_snapshot_lineage_idx" ON "environment_node_config_snapshot" ("node_type","node_lineage_id");--> statement-breakpoint
CREATE INDEX "environment_node_config_snapshot_deployment_id_idx" ON "environment_node_config_snapshot" ("environment_deployment_id");--> statement-breakpoint
CREATE INDEX "environment_node_introduction_lineage_idx" ON "environment_node_introduction" ("node_type","node_lineage_id");--> statement-breakpoint
CREATE INDEX "environment_node_introduction_organization_idx" ON "environment_node_introduction" ("organization_id");--> statement-breakpoint
CREATE INDEX "teardown_attempt_organization_id_idx" ON "teardown_attempt" ("organization_id");--> statement-breakpoint
CREATE INDEX "teardown_attempt_project_id_idx" ON "teardown_attempt" ("project_id");--> statement-breakpoint
CREATE INDEX "teardown_attempt_environment_id_idx" ON "teardown_attempt" ("environment_id");--> statement-breakpoint
CREATE UNIQUE INDEX "teardown_attempt_retry_of_idx" ON "teardown_attempt" ("retry_of_attempt_id") WHERE "retry_of_attempt_id" is not null;--> statement-breakpoint
CREATE UNIQUE INDEX "teardown_attempt_one_active_environment_idx" ON "teardown_attempt" ("environment_id") WHERE "status" in ('pending', 'running')
          and "scope" = 'environment'
          and "environment_id" is not null;--> statement-breakpoint
CREATE UNIQUE INDEX "teardown_attempt_one_active_project_idx" ON "teardown_attempt" ("project_id") WHERE "status" in ('pending', 'running')
          and "scope" = 'project'
          and "project_id" is not null;--> statement-breakpoint
CREATE UNIQUE INDEX "teardown_attempt_one_active_organization_idx" ON "teardown_attempt" ("organization_id") WHERE "status" in ('pending', 'running')
          and "scope" = 'organization';--> statement-breakpoint
CREATE UNIQUE INDEX "teardown_attempt_inngest_run_uidx" ON "teardown_attempt" ("inngest_run_id") WHERE "inngest_run_id" is not null;--> statement-breakpoint
CREATE INDEX "volume_remove_attempt_organization_id_idx" ON "volume_remove_attempt" ("organization_id");--> statement-breakpoint
CREATE INDEX "volume_remove_attempt_environment_id_idx" ON "volume_remove_attempt" ("environment_id");--> statement-breakpoint
CREATE INDEX "volume_remove_attempt_resource_idx" ON "volume_remove_attempt" ("environment_resource_id","created_at");--> statement-breakpoint
CREATE UNIQUE INDEX "volume_remove_attempt_retry_of_idx" ON "volume_remove_attempt" ("retry_of_attempt_id") WHERE "retry_of_attempt_id" is not null;--> statement-breakpoint
CREATE UNIQUE INDEX "volume_remove_attempt_one_active_resource_idx" ON "volume_remove_attempt" ("environment_resource_id") WHERE "status" in ('pending', 'running')
          and "environment_resource_id" is not null;--> statement-breakpoint
CREATE UNIQUE INDEX "volume_remove_attempt_inngest_run_uidx" ON "volume_remove_attempt" ("inngest_run_id") WHERE "inngest_run_id" is not null;--> statement-breakpoint
CREATE UNIQUE INDEX "machine_enrollment_token_hash_idx" ON "machine_enrollment_token" ("token_hash");--> statement-breakpoint
CREATE INDEX "machine_enrollment_token_organization_idx" ON "machine_enrollment_token" ("organization_id");--> statement-breakpoint
CREATE UNIQUE INDEX "machine_remove_attempt_inngest_run_uidx" ON "machine_remove_attempt" ("inngest_run_id") WHERE "inngest_run_id" is not null;--> statement-breakpoint
CREATE UNIQUE INDEX "machine_remove_attempt_one_active_org_machine_idx" ON "machine_remove_attempt" ("organization_id","machine_id") WHERE "state" in ('pending', 'running');--> statement-breakpoint
CREATE INDEX "organization_machine_machine_idx" ON "organization_machine" ("machine_id");--> statement-breakpoint
CREATE UNIQUE INDEX "organization_machine_one_dial_entry_idx" ON "organization_machine" ("organization_id") WHERE "is_dial_entry";--> statement-breakpoint
CREATE INDEX "github_branch_projection_head_idx" ON "github_branch_projection" ("installation_id","repository_id","evaluated_head_sha");--> statement-breakpoint
CREATE INDEX "github_check_suite_projection_head_idx" ON "github_check_suite_projection" ("installation_id","repository_id","head_sha");--> statement-breakpoint
CREATE INDEX "github_check_suite_projection_unpublished_idx" ON "github_check_suite_projection" ("updated_at") WHERE "published_revision" < "transition_revision";--> statement-breakpoint
CREATE INDEX "github_environment_trigger_environment_idx" ON "github_environment_trigger" ("environment_id","created_at");--> statement-breakpoint
CREATE INDEX "github_environment_trigger_pending_idx" ON "github_environment_trigger" ("created_at") WHERE "publish_state" = 'pending';--> statement-breakpoint
CREATE INDEX "github_installation_user_idx" ON "github_installation" ("user_id");--> statement-breakpoint
CREATE INDEX "github_installation_installation_id_idx" ON "github_installation" ("installation_id");--> statement-breakpoint
CREATE INDEX "github_repository_cache_installation_idx" ON "github_repository_cache" ("installation_id");--> statement-breakpoint
CREATE INDEX "github_repository_cache_user_idx" ON "github_repository_cache" ("user_id");--> statement-breakpoint
CREATE INDEX "github_repository_cache_full_name_idx" ON "github_repository_cache" ("full_name");--> statement-breakpoint
CREATE INDEX "github_webhook_delivery_processing_idx" ON "github_webhook_delivery" ("processing_state","receipt_sequence");--> statement-breakpoint
CREATE INDEX "github_webhook_delivery_identity_idx" ON "github_webhook_delivery" ("installation_id","repository_id");--> statement-breakpoint
CREATE UNIQUE INDEX "github_webhook_delivery_processing_run_id_idx" ON "github_webhook_delivery" ("processing_run_id") WHERE "processing_run_id" is not null;--> statement-breakpoint
ALTER TABLE "account" ADD CONSTRAINT "account_user_id_user_id_fkey" FOREIGN KEY ("user_id") REFERENCES "user"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "invitation" ADD CONSTRAINT "invitation_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "invitation" ADD CONSTRAINT "invitation_inviter_id_user_id_fkey" FOREIGN KEY ("inviter_id") REFERENCES "user"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "member" ADD CONSTRAINT "member_user_id_user_id_fkey" FOREIGN KEY ("user_id") REFERENCES "user"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "member" ADD CONSTRAINT "member_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "session" ADD CONSTRAINT "session_user_id_user_id_fkey" FOREIGN KEY ("user_id") REFERENCES "user"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment" ADD CONSTRAINT "environment_project_id_project_id_fkey" FOREIGN KEY ("project_id") REFERENCES "project"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment" ADD CONSTRAINT "environment_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment" ADD CONSTRAINT "environment_IqLeTQthFKCX_fkey" FOREIGN KEY ("organization_id","project_id") REFERENCES "project"("organization_id","id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "project" ADD CONSTRAINT "project_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "user_project_preference" ADD CONSTRAINT "user_project_preference_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "user_project_preference" ADD CONSTRAINT "user_project_preference_user_id_user_id_fkey" FOREIGN KEY ("user_id") REFERENCES "user"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "user_project_preference" ADD CONSTRAINT "user_project_preference_project_id_project_id_fkey" FOREIGN KEY ("project_id") REFERENCES "project"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "user_project_preference" ADD CONSTRAINT "user_project_preference_environment_id_environment_id_fkey" FOREIGN KEY ("environment_id") REFERENCES "environment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "config_key" ADD CONSTRAINT "config_key_project_id_project_id_fkey" FOREIGN KEY ("project_id") REFERENCES "project"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "config_key" ADD CONSTRAINT "config_key_service_lineage_id_service_lineage_id_fkey" FOREIGN KEY ("service_lineage_id") REFERENCES "service_lineage"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "config_key" ADD CONSTRAINT "config_key_Z9IFDoZl4bce_fkey" FOREIGN KEY ("variable_group_lineage_id") REFERENCES "variable_group_lineage"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "config_key" ADD CONSTRAINT "config_key_5VUyMPmokT4w_fkey" FOREIGN KEY ("project_id","service_lineage_id") REFERENCES "service_lineage"("project_id","id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "config_key" ADD CONSTRAINT "config_key_XJjCzTc9crVY_fkey" FOREIGN KEY ("project_id","variable_group_lineage_id") REFERENCES "variable_group_lineage"("project_id","id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "config_value" ADD CONSTRAINT "config_value_project_id_project_id_fkey" FOREIGN KEY ("project_id") REFERENCES "project"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "config_value" ADD CONSTRAINT "config_value_config_key_id_config_key_id_fkey" FOREIGN KEY ("config_key_id") REFERENCES "config_key"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "config_value" ADD CONSTRAINT "config_value_environment_id_environment_id_fkey" FOREIGN KEY ("environment_id") REFERENCES "environment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "config_value" ADD CONSTRAINT "config_value_PIOwCmc9UgtM_fkey" FOREIGN KEY ("project_id","config_key_id") REFERENCES "config_key"("project_id","id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "config_value" ADD CONSTRAINT "config_value_rxYFy3WfPQlC_fkey" FOREIGN KEY ("project_id","environment_id") REFERENCES "environment"("project_id","id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_canvas_node_position" ADD CONSTRAINT "environment_canvas_node_position_4ir4xbHMhQ0b_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_canvas_node_position" ADD CONSTRAINT "environment_canvas_node_position_DP4bUoilqg6D_fkey" FOREIGN KEY ("environment_id") REFERENCES "environment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_resource" ADD CONSTRAINT "environment_resource_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_resource" ADD CONSTRAINT "environment_resource_project_id_project_id_fkey" FOREIGN KEY ("project_id") REFERENCES "project"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_resource" ADD CONSTRAINT "environment_resource_environment_id_environment_id_fkey" FOREIGN KEY ("environment_id") REFERENCES "environment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_resource" ADD CONSTRAINT "environment_resource_lineage_id_resource_lineage_id_fkey" FOREIGN KEY ("lineage_id") REFERENCES "resource_lineage"("id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "environment_resource" ADD CONSTRAINT "environment_resource_nABwUGd5rp5C_fkey" FOREIGN KEY ("variable_group_id") REFERENCES "environment_variable_group"("id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "environment_resource" ADD CONSTRAINT "environment_resource_krkfQwOLhWDI_fkey" FOREIGN KEY ("project_id","environment_id") REFERENCES "environment"("project_id","id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_resource" ADD CONSTRAINT "environment_resource_zQ2aoOGRLIOR_fkey" FOREIGN KEY ("project_id","lineage_id") REFERENCES "resource_lineage"("project_id","id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "environment_resource" ADD CONSTRAINT "environment_resource_s2Nqmux9EkpK_fkey" FOREIGN KEY ("project_id","variable_group_id") REFERENCES "environment_variable_group"("project_id","id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "environment_variable_group" ADD CONSTRAINT "environment_variable_group_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_variable_group" ADD CONSTRAINT "environment_variable_group_project_id_project_id_fkey" FOREIGN KEY ("project_id") REFERENCES "project"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_variable_group" ADD CONSTRAINT "environment_variable_group_environment_id_environment_id_fkey" FOREIGN KEY ("environment_id") REFERENCES "environment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_variable_group" ADD CONSTRAINT "environment_variable_group_hiba8dNGKiwl_fkey" FOREIGN KEY ("lineage_id") REFERENCES "variable_group_lineage"("id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "environment_variable_group" ADD CONSTRAINT "environment_variable_group_oOF37dttct8c_fkey" FOREIGN KEY ("project_id","environment_id") REFERENCES "environment"("project_id","id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_variable_group" ADD CONSTRAINT "environment_variable_group_pbUjmr26aBQN_fkey" FOREIGN KEY ("project_id","lineage_id") REFERENCES "variable_group_lineage"("project_id","id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "resource_lineage" ADD CONSTRAINT "resource_lineage_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "resource_lineage" ADD CONSTRAINT "resource_lineage_project_id_project_id_fkey" FOREIGN KEY ("project_id") REFERENCES "project"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "service" ADD CONSTRAINT "service_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "service" ADD CONSTRAINT "service_project_id_project_id_fkey" FOREIGN KEY ("project_id") REFERENCES "project"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "service" ADD CONSTRAINT "service_environment_id_environment_id_fkey" FOREIGN KEY ("environment_id") REFERENCES "environment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "service" ADD CONSTRAINT "service_lineage_id_service_lineage_id_fkey" FOREIGN KEY ("lineage_id") REFERENCES "service_lineage"("id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "service" ADD CONSTRAINT "service_bvUAv6STak07_fkey" FOREIGN KEY ("project_id","environment_id") REFERENCES "environment"("project_id","id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "service" ADD CONSTRAINT "service_1xRYD1MXFhcT_fkey" FOREIGN KEY ("project_id","lineage_id") REFERENCES "service_lineage"("project_id","id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "service_lineage" ADD CONSTRAINT "service_lineage_project_id_project_id_fkey" FOREIGN KEY ("project_id") REFERENCES "project"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "service_registry_credential" ADD CONSTRAINT "service_registry_credential_service_id_service_id_fkey" FOREIGN KEY ("service_id") REFERENCES "service"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "service_variable_group_attachment" ADD CONSTRAINT "service_variable_group_attachment_NYXbiK6fRK1W_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "service_variable_group_attachment" ADD CONSTRAINT "service_variable_group_attachment_service_id_service_id_fkey" FOREIGN KEY ("service_id") REFERENCES "service"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "service_variable_group_attachment" ADD CONSTRAINT "service_variable_group_attachment_9dQ3j1TvmUKG_fkey" FOREIGN KEY ("variable_group_id") REFERENCES "environment_variable_group"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "service_volume_attachment" ADD CONSTRAINT "service_volume_attachment_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "service_volume_attachment" ADD CONSTRAINT "service_volume_attachment_project_id_project_id_fkey" FOREIGN KEY ("project_id") REFERENCES "project"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "service_volume_attachment" ADD CONSTRAINT "service_volume_attachment_environment_id_environment_id_fkey" FOREIGN KEY ("environment_id") REFERENCES "environment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "service_volume_attachment" ADD CONSTRAINT "service_volume_attachment_kFuCCx6h3Bu6_fkey" FOREIGN KEY ("environment_id","service_id") REFERENCES "service"("environment_id","id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "service_volume_attachment" ADD CONSTRAINT "service_volume_attachment_x8hGiuXiEjZ1_fkey" FOREIGN KEY ("environment_id","volume_resource_id") REFERENCES "environment_resource"("environment_id","id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "variable" ADD CONSTRAINT "variable_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "variable" ADD CONSTRAINT "variable_project_id_project_id_fkey" FOREIGN KEY ("project_id") REFERENCES "project"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "variable" ADD CONSTRAINT "variable_service_id_service_id_fkey" FOREIGN KEY ("service_id") REFERENCES "service"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "variable" ADD CONSTRAINT "variable_variable_group_id_environment_variable_group_id_fkey" FOREIGN KEY ("variable_group_id") REFERENCES "environment_variable_group"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "variable" ADD CONSTRAINT "variable_config_key_id_config_key_id_fkey" FOREIGN KEY ("config_key_id") REFERENCES "config_key"("id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "variable" ADD CONSTRAINT "variable_project_id_config_key_id_config_key_project_id_id_fkey" FOREIGN KEY ("project_id","config_key_id") REFERENCES "config_key"("project_id","id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "variable" ADD CONSTRAINT "variable_project_id_service_id_service_project_id_id_fkey" FOREIGN KEY ("project_id","service_id") REFERENCES "service"("project_id","id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "variable" ADD CONSTRAINT "variable_OCJdPfzRCIC2_fkey" FOREIGN KEY ("project_id","variable_group_id") REFERENCES "environment_variable_group"("project_id","id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "variable_group_lineage" ADD CONSTRAINT "variable_group_lineage_project_id_project_id_fkey" FOREIGN KEY ("project_id") REFERENCES "project"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "variable_secret" ADD CONSTRAINT "variable_secret_variable_id_variable_id_fkey" FOREIGN KEY ("variable_id") REFERENCES "variable"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_deployment" ADD CONSTRAINT "environment_deployment_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_deployment" ADD CONSTRAINT "environment_deployment_environment_id_environment_id_fkey" FOREIGN KEY ("environment_id") REFERENCES "environment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_deployment" ADD CONSTRAINT "environment_deployment_Lofdv4M9OTIQ_fkey" FOREIGN KEY ("retry_of_deployment_id") REFERENCES "environment_deployment"("id");--> statement-breakpoint
ALTER TABLE "environment_deployment" ADD CONSTRAINT "environment_deployment_92k6WdAbc3AZ_fkey" FOREIGN KEY ("environment_id","saved_state_snapshot_id") REFERENCES "environment_saved_state_snapshot"("environment_id","id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "environment_deployment_secret" ADD CONSTRAINT "environment_deployment_secret_IIXCU5ibjYPq_fkey" FOREIGN KEY ("environment_deployment_id") REFERENCES "environment_deployment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_saved_state_snapshot" ADD CONSTRAINT "environment_saved_state_snapshot_QseWcaivJkoo_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_saved_state_snapshot" ADD CONSTRAINT "environment_saved_state_snapshot_pYS3zmT4RKuQ_fkey" FOREIGN KEY ("environment_id") REFERENCES "environment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_saved_state_snapshot" ADD CONSTRAINT "environment_saved_state_snapshot_actor_id_user_id_fkey" FOREIGN KEY ("actor_id") REFERENCES "user"("id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "core_operation_event" ADD CONSTRAINT "core_operation_event_watch_id_core_operation_watch_id_fkey" FOREIGN KEY ("watch_id") REFERENCES "core_operation_watch"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "core_operation_watch" ADD CONSTRAINT "core_operation_watch_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "destructive_volume_attempt" ADD CONSTRAINT "destructive_volume_attempt_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "destructive_volume_attempt" ADD CONSTRAINT "destructive_volume_attempt_cVJ73enJtw9z_fkey" FOREIGN KEY ("environment_deployment_id") REFERENCES "environment_deployment"("id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "destructive_volume_attempt" ADD CONSTRAINT "destructive_volume_attempt_ep248mgRYD0C_fkey" FOREIGN KEY ("retry_of_attempt_id") REFERENCES "destructive_volume_attempt"("id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "environment_node_config_snapshot" ADD CONSTRAINT "environment_node_config_snapshot_3y0HYOFWmCwQ_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_node_config_snapshot" ADD CONSTRAINT "environment_node_config_snapshot_CUaPaujMBOIs_fkey" FOREIGN KEY ("environment_deployment_id") REFERENCES "environment_deployment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_node_config_snapshot" ADD CONSTRAINT "environment_node_config_snapshot_D4DPm1gvv2Di_fkey" FOREIGN KEY ("environment_id") REFERENCES "environment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_node_config_snapshot_secret" ADD CONSTRAINT "environment_node_config_snapshot_secret_j5H7VPFuELrf_fkey" FOREIGN KEY ("snapshot_id") REFERENCES "environment_node_config_snapshot"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_node_introduction" ADD CONSTRAINT "environment_node_introduction_aSA9HOqVYGRT_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_node_introduction" ADD CONSTRAINT "environment_node_introduction_YPW21fX4uIUZ_fkey" FOREIGN KEY ("environment_id") REFERENCES "environment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_node_introduction_secret" ADD CONSTRAINT "environment_node_introduction_secret_sC7ALWFOnE38_fkey" FOREIGN KEY ("environment_id","node_type","node_id") REFERENCES "environment_node_introduction"("environment_id","node_type","node_id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "organization_pairing" ADD CONSTRAINT "organization_pairing_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "teardown_attempt" ADD CONSTRAINT "teardown_attempt_requested_by_user_id_user_id_fkey" FOREIGN KEY ("requested_by_user_id") REFERENCES "user"("id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "teardown_attempt" ADD CONSTRAINT "teardown_attempt_retry_of_attempt_id_teardown_attempt_id_fkey" FOREIGN KEY ("retry_of_attempt_id") REFERENCES "teardown_attempt"("id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "volume_remove_attempt" ADD CONSTRAINT "volume_remove_attempt_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "volume_remove_attempt" ADD CONSTRAINT "volume_remove_attempt_requested_by_user_id_user_id_fkey" FOREIGN KEY ("requested_by_user_id") REFERENCES "user"("id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "volume_remove_attempt" ADD CONSTRAINT "volume_remove_attempt_environment_id_environment_id_fkey" FOREIGN KEY ("environment_id") REFERENCES "environment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "volume_remove_attempt" ADD CONSTRAINT "volume_remove_attempt_FQapB8QhQW1M_fkey" FOREIGN KEY ("retry_of_attempt_id") REFERENCES "volume_remove_attempt"("id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "machine_enrollment_token" ADD CONSTRAINT "machine_enrollment_token_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "machine_enrollment_token" ADD CONSTRAINT "machine_enrollment_token_created_by_user_id_user_id_fkey" FOREIGN KEY ("created_by_user_id") REFERENCES "user"("id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "machine_remove_attempt" ADD CONSTRAINT "machine_remove_attempt_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "machine_remove_attempt" ADD CONSTRAINT "machine_remove_attempt_requested_by_user_id_user_id_fkey" FOREIGN KEY ("requested_by_user_id") REFERENCES "user"("id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "organization_machine" ADD CONSTRAINT "organization_machine_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "github_branch_projection" ADD CONSTRAINT "github_branch_projection_Udo5xaDQmVxR_fkey" FOREIGN KEY ("last_delivery_id","last_receipt_sequence") REFERENCES "github_webhook_delivery"("delivery_id","receipt_sequence") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "github_check_suite_projection" ADD CONSTRAINT "github_check_suite_projection_9FLSHaRf6WgI_fkey" FOREIGN KEY ("last_delivery_id","last_receipt_sequence") REFERENCES "github_webhook_delivery"("delivery_id","receipt_sequence") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "github_environment_trigger" ADD CONSTRAINT "github_environment_trigger_environment_id_environment_id_fkey" FOREIGN KEY ("environment_id") REFERENCES "environment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "github_environment_trigger" ADD CONSTRAINT "github_environment_trigger_dspFptTlur3h_fkey" FOREIGN KEY ("source_delivery_id","source_receipt_sequence") REFERENCES "github_webhook_delivery"("delivery_id","receipt_sequence") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "github_installation" ADD CONSTRAINT "github_installation_user_id_user_id_fkey" FOREIGN KEY ("user_id") REFERENCES "user"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "github_repository_cache" ADD CONSTRAINT "github_repository_cache_user_id_user_id_fkey" FOREIGN KEY ("user_id") REFERENCES "user"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "organization_billing_state" ADD CONSTRAINT "organization_billing_state_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;
--> statement-breakpoint
-- Electric replication settings are not represented in Drizzle snapshots.
ALTER TABLE "destructive_volume_attempt" REPLICA IDENTITY FULL;

--> statement-breakpoint
ALTER TABLE "environment" REPLICA IDENTITY FULL;

--> statement-breakpoint
ALTER TABLE "environment_canvas_node_position" REPLICA IDENTITY FULL;

--> statement-breakpoint
ALTER TABLE "environment_deployment" REPLICA IDENTITY FULL;

--> statement-breakpoint
ALTER TABLE "environment_node_config_snapshot" REPLICA IDENTITY FULL;

--> statement-breakpoint
ALTER TABLE "environment_resource" REPLICA IDENTITY FULL;

--> statement-breakpoint
ALTER TABLE "environment_variable_group" REPLICA IDENTITY FULL;

--> statement-breakpoint
ALTER TABLE "github_repository_cache" REPLICA IDENTITY FULL;

--> statement-breakpoint
ALTER TABLE "organization" REPLICA IDENTITY FULL;

--> statement-breakpoint
ALTER TABLE "project" REPLICA IDENTITY FULL;

--> statement-breakpoint
ALTER TABLE "resource_lineage" REPLICA IDENTITY FULL;

--> statement-breakpoint
ALTER TABLE "service" REPLICA IDENTITY FULL;

--> statement-breakpoint
ALTER TABLE "service_variable_group_attachment" REPLICA IDENTITY FULL;

--> statement-breakpoint
ALTER TABLE "service_volume_attachment" REPLICA IDENTITY FULL;

--> statement-breakpoint
ALTER TABLE "user_project_preference" REPLICA IDENTITY FULL;

--> statement-breakpoint
ALTER TABLE "variable" REPLICA IDENTITY FULL;

--> statement-breakpoint
ALTER TABLE "environment_node_introduction" REPLICA IDENTITY FULL;

--> statement-breakpoint
ALTER TABLE "environment_saved_state_snapshot" REPLICA IDENTITY FULL;
