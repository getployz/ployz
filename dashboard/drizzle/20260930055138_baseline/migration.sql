CREATE TABLE "organization" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"name" text NOT NULL,
	"slug" text NOT NULL UNIQUE,
	"logo" text,
	"metadata" text,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "organization_change" (
	"seq" bigint PRIMARY KEY GENERATED ALWAYS AS IDENTITY (sequence name "organization_change_seq_seq" INCREMENT BY 1 MINVALUE 1 MAXVALUE 9223372036854775807 START WITH 1 CACHE 1),
	"xid" xid8 DEFAULT pg_current_xact_id() NOT NULL,
	"organization_id" uuid NOT NULL,
	"source_table" text NOT NULL,
	"changed_ids" text[] NOT NULL,
	"deleted_ids" text[] NOT NULL,
	"all_rows" boolean NOT NULL,
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
CREATE TABLE "device_code" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	"device_code" text NOT NULL UNIQUE,
	"user_code" text NOT NULL UNIQUE,
	"user_id" uuid,
	"expires_at" timestamp with time zone NOT NULL,
	"status" text NOT NULL,
	"last_polled_at" timestamp with time zone,
	"polling_interval" integer,
	"client_id" text,
	"scope" text
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
CREATE TABLE "organization_token" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"organization_id" uuid NOT NULL,
	"user_id" uuid NOT NULL,
	"name" text NOT NULL,
	"secret_hash" text NOT NULL UNIQUE,
	"expires_at" timestamp with time zone NOT NULL
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
	"image" text,
	"open_started_deployments" boolean DEFAULT true NOT NULL
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
CREATE TABLE "organization_pairing" (
	"organization_id" uuid PRIMARY KEY,
	"encrypted_pairing_secret" jsonb NOT NULL,
	"removal_started_at" timestamp with time zone,
	"removal_endpoints" jsonb,
	"founder_public_key" text,
	"founder_claim_machine_id" text NOT NULL,
	"founder_machine_id" text,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "organization_pairing_removal_shape_check" CHECK (
      ("removal_started_at" is null and "removal_endpoints" is null)
      or ("removal_started_at" is not null and "removal_endpoints" is not null and jsonb_typeof("removal_endpoints") = 'array')
    ),
	CONSTRAINT "organization_pairing_state_check" CHECK ("founder_public_key" is not null or "founder_machine_id" is not null),
	CONSTRAINT "organization_pairing_founder_claim_machine_id_check" CHECK ("founder_claim_machine_id" ~ '^[0-9a-f]{32}$'),
	CONSTRAINT "organization_pairing_founder_machine_id_check" CHECK ("founder_machine_id" is null or "founder_machine_id" ~ '^[0-9a-f]{32}$')
);
--> statement-breakpoint
CREATE TABLE "enrollment_allocation" (
	"organization_id" uuid,
	"cluster_key" text,
	"assignments" jsonb NOT NULL,
	CONSTRAINT "enrollment_allocation_pkey" PRIMARY KEY("organization_id","cluster_key"),
	CONSTRAINT "enrollment_allocation_cluster_key_check" CHECK ("cluster_key" ~ '^[0-9a-f]{64}$'),
	CONSTRAINT "enrollment_allocation_assignments_check" CHECK (jsonb_typeof("assignments") = 'array')
);
--> statement-breakpoint
CREATE TABLE "machine_enrollment_token" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"token_hash" text NOT NULL,
	"created_by_user_id" uuid NOT NULL,
	"expires_at" timestamp with time zone NOT NULL,
	"joined_machine_id" text,
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
	"cluster_key" text NOT NULL,
	"encrypted_capability" jsonb NOT NULL,
	"is_dial_entry" boolean DEFAULT false NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "organization_machine_pkey" PRIMARY KEY("organization_id","machine_id"),
	CONSTRAINT "organization_machine_cluster_key_check" CHECK ("cluster_key" ~ '^[0-9a-f]{64}$'),
	CONSTRAINT "organization_machine_id_format_check" CHECK ("machine_id" ~ '^[0-9a-f]{32}$')
);
--> statement-breakpoint
CREATE TABLE "server_access" (
	"organization_id" uuid,
	"machine_id" text,
	"credential_id" uuid,
	"credential_kind" text NOT NULL,
	"user_id" uuid NOT NULL,
	"encrypted_capability" jsonb,
	"revoked_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "server_access_pkey" PRIMARY KEY("organization_id","credential_id","machine_id"),
	CONSTRAINT "server_access_credential_kind_check" CHECK ("credential_kind" in ('session', 'token')),
	CONSTRAINT "server_access_revoked_shape_check" CHECK (("revoked_at" is null) = ("encrypted_capability" is not null))
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
CREATE TABLE "organization_billing_state" (
	"organization_id" uuid PRIMARY KEY,
	"active_subscription_id" text,
	"current_period_end" timestamp with time zone,
	"cancel_at_period_end" boolean DEFAULT false NOT NULL,
	"has_active_subscription" boolean DEFAULT false NOT NULL,
	"synced_at" timestamp with time zone DEFAULT now() NOT NULL,
	"source_updated_at" timestamp with time zone,
	CONSTRAINT "organization_billing_state_active_fields_check" CHECK ((
        "has_active_subscription" = false
        OR (
          "active_subscription_id" IS NOT NULL
          AND "current_period_end" IS NOT NULL
        )
      ))
);
--> statement-breakpoint
CREATE TABLE "organization_cluster_domain" (
	"organization_id" uuid PRIMARY KEY,
	"endpoint" text NOT NULL,
	"name" text NOT NULL,
	"encrypted_token" jsonb NOT NULL,
	"reserved_at" timestamp with time zone NOT NULL,
	"lease_renewed_at" timestamp with time zone NOT NULL,
	"records_synced_at" timestamp with time zone,
	"traffic" jsonb,
	"checked_at" timestamp with time zone,
	"encrypted_certificate_private_key" jsonb,
	"certificate_chain" text,
	"certificate_not_after" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "organization_cluster_domain_traffic_check" CHECK ("traffic" is null or "traffic"->>'kind' in ('no_servers', 'no_public_ip', 'probed')),
	CONSTRAINT "organization_cluster_domain_certificate_check" CHECK (num_nulls("encrypted_certificate_private_key", "certificate_chain", "certificate_not_after") in (0, 3))
);
--> statement-breakpoint
CREATE TABLE "deployment_run" (
	"run_id" text PRIMARY KEY,
	"organization_id" uuid NOT NULL,
	"deployment_id" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
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
CREATE INDEX "organization_change_organization_id_xid_idx" ON "organization_change" ("organization_id","xid");--> statement-breakpoint
CREATE INDEX "organization_change_xid_idx" ON "organization_change" ("xid");--> statement-breakpoint
CREATE INDEX "account_user_id_idx" ON "account" ("user_id");--> statement-breakpoint
CREATE INDEX "account_provider_account_idx" ON "account" ("provider_id","account_id");--> statement-breakpoint
CREATE INDEX "invitation_organization_id_idx" ON "invitation" ("organization_id");--> statement-breakpoint
CREATE INDEX "invitation_email_idx" ON "invitation" ("email");--> statement-breakpoint
CREATE INDEX "organization_token_organization_id_idx" ON "organization_token" ("organization_id");--> statement-breakpoint
CREATE INDEX "session_user_id_idx" ON "session" ("user_id");--> statement-breakpoint
CREATE INDEX "verification_identifier_idx" ON "verification" ("identifier");--> statement-breakpoint
CREATE UNIQUE INDEX "machine_enrollment_token_hash_idx" ON "machine_enrollment_token" ("token_hash");--> statement-breakpoint
CREATE INDEX "machine_enrollment_token_organization_idx" ON "machine_enrollment_token" ("organization_id");--> statement-breakpoint
CREATE UNIQUE INDEX "machine_remove_attempt_inngest_run_uidx" ON "machine_remove_attempt" ("inngest_run_id") WHERE "inngest_run_id" is not null;--> statement-breakpoint
CREATE UNIQUE INDEX "machine_remove_attempt_one_active_org_machine_idx" ON "machine_remove_attempt" ("organization_id","machine_id") WHERE "state" in ('pending', 'running');--> statement-breakpoint
CREATE INDEX "organization_machine_machine_idx" ON "organization_machine" ("machine_id");--> statement-breakpoint
CREATE UNIQUE INDEX "organization_machine_one_dial_entry_idx" ON "organization_machine" ("organization_id") WHERE "is_dial_entry";--> statement-breakpoint
CREATE INDEX "server_access_credential_idx" ON "server_access" ("credential_id");--> statement-breakpoint
CREATE INDEX "github_installation_user_idx" ON "github_installation" ("user_id");--> statement-breakpoint
CREATE INDEX "github_installation_installation_id_idx" ON "github_installation" ("installation_id");--> statement-breakpoint
CREATE INDEX "github_repository_cache_installation_idx" ON "github_repository_cache" ("installation_id");--> statement-breakpoint
CREATE INDEX "github_repository_cache_user_idx" ON "github_repository_cache" ("user_id");--> statement-breakpoint
CREATE INDEX "github_repository_cache_full_name_idx" ON "github_repository_cache" ("full_name");--> statement-breakpoint
CREATE INDEX "upload_chunk_created_at_idx" ON "upload_chunk" ("created_at");--> statement-breakpoint
CREATE INDEX "environment_canvas_node_position_environment_id_idx" ON "environment_canvas_node_position" ("environment_id");--> statement-breakpoint
CREATE INDEX "environment_canvas_node_position_organization_id_idx" ON "environment_canvas_node_position" ("organization_id");--> statement-breakpoint
CREATE INDEX "environment_canvas_node_position_resource_lookup_idx" ON "environment_canvas_node_position" ("resource_type","resource_id");--> statement-breakpoint
ALTER TABLE "account" ADD CONSTRAINT "account_user_id_user_id_fkey" FOREIGN KEY ("user_id") REFERENCES "user"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "device_code" ADD CONSTRAINT "device_code_user_id_user_id_fkey" FOREIGN KEY ("user_id") REFERENCES "user"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "invitation" ADD CONSTRAINT "invitation_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "invitation" ADD CONSTRAINT "invitation_inviter_id_user_id_fkey" FOREIGN KEY ("inviter_id") REFERENCES "user"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "member" ADD CONSTRAINT "member_user_id_user_id_fkey" FOREIGN KEY ("user_id") REFERENCES "user"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "member" ADD CONSTRAINT "member_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "organization_token" ADD CONSTRAINT "organization_token_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "organization_token" ADD CONSTRAINT "organization_token_user_id_user_id_fkey" FOREIGN KEY ("user_id") REFERENCES "user"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "session" ADD CONSTRAINT "session_user_id_user_id_fkey" FOREIGN KEY ("user_id") REFERENCES "user"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "organization_pairing" ADD CONSTRAINT "organization_pairing_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "enrollment_allocation" ADD CONSTRAINT "enrollment_allocation_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "machine_enrollment_token" ADD CONSTRAINT "machine_enrollment_token_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "machine_enrollment_token" ADD CONSTRAINT "machine_enrollment_token_created_by_user_id_user_id_fkey" FOREIGN KEY ("created_by_user_id") REFERENCES "user"("id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "machine_remove_attempt" ADD CONSTRAINT "machine_remove_attempt_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "machine_remove_attempt" ADD CONSTRAINT "machine_remove_attempt_requested_by_user_id_user_id_fkey" FOREIGN KEY ("requested_by_user_id") REFERENCES "user"("id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "organization_machine" ADD CONSTRAINT "organization_machine_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "server_access" ADD CONSTRAINT "server_access_organization_machine_fkey" FOREIGN KEY ("organization_id","machine_id") REFERENCES "organization_machine"("organization_id","machine_id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "github_installation" ADD CONSTRAINT "github_installation_user_id_user_id_fkey" FOREIGN KEY ("user_id") REFERENCES "user"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "github_repository_cache" ADD CONSTRAINT "github_repository_cache_user_id_user_id_fkey" FOREIGN KEY ("user_id") REFERENCES "user"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "organization_billing_state" ADD CONSTRAINT "organization_billing_state_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "organization_cluster_domain" ADD CONSTRAINT "organization_cluster_domain_eJqKfNjJBhUj_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "deployment_run" ADD CONSTRAINT "deployment_run_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "upload_chunk" ADD CONSTRAINT "upload_chunk_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_canvas_node_position" ADD CONSTRAINT "environment_canvas_node_position_4ir4xbHMhQ0b_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;
--> statement-breakpoint

-- Config Store tables keep their Organization as text (their SQL also runs on SQLite), so the log casts it.
CREATE OR REPLACE FUNCTION organization_change_log() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
  key_expression text;
  old_keys text := 'SELECT NULL::uuid AS organization_id, NULL::text AS key WHERE false';
  new_keys text := old_keys;
BEGIN
  SELECT string_agg(format('%I::text', key_column), ' || '':'' || ')
    INTO key_expression FROM unnest(TG_ARGV[1:TG_NARGS - 1]) AS key_column;
  IF TG_OP IN ('INSERT', 'UPDATE') THEN
    new_keys := format('SELECT %I::uuid AS organization_id, %s AS key FROM new_rows', TG_ARGV[0], key_expression);
  END IF;
  IF TG_OP IN ('UPDATE', 'DELETE') THEN
    old_keys := format('SELECT %I::uuid AS organization_id, %s AS key FROM old_rows', TG_ARGV[0], key_expression);
  END IF;
  EXECUTE format($sql$
    INSERT INTO organization_change (organization_id, source_table, changed_ids, deleted_ids, all_rows)
    SELECT organization_id, %L,
      CASE WHEN count(*) > 100 THEN '{}' ELSE coalesce(array_agg(key) FILTER (WHERE NOT deleted), '{}') END,
      CASE WHEN count(*) > 100 THEN '{}' ELSE coalesce(array_agg(key) FILTER (WHERE deleted), '{}') END,
      count(*) > 100
    FROM (
      SELECT DISTINCT organization_id, key, false AS deleted FROM (%s) changed
      UNION ALL
      (SELECT organization_id, key, true FROM (%s) gone EXCEPT SELECT organization_id, key, true FROM (%s) changed)
    ) keys
    GROUP BY organization_id
  $sql$, TG_TABLE_NAME, new_keys, old_keys, new_keys);
  RETURN NULL;
END
$$;
--> statement-breakpoint
-- Transition tables allow one event per trigger, so each table gets three.
CREATE FUNCTION organization_change_attach(target regclass, organization_column text, VARIADIC key_columns text[])
RETURNS void LANGUAGE plpgsql AS $$
DECLARE
  arguments text := (SELECT string_agg(quote_literal(argument), ', ') FROM unnest(organization_column || key_columns) AS argument);
BEGIN
  EXECUTE format('CREATE TRIGGER organization_change_insert AFTER INSERT ON %s REFERENCING NEW TABLE AS new_rows
    FOR EACH STATEMENT EXECUTE FUNCTION organization_change_log(%s)', target, arguments);
  EXECUTE format('CREATE TRIGGER organization_change_update AFTER UPDATE ON %s REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
    FOR EACH STATEMENT EXECUTE FUNCTION organization_change_log(%s)', target, arguments);
  EXECUTE format('CREATE TRIGGER organization_change_delete AFTER DELETE ON %s REFERENCING OLD TABLE AS old_rows
    FOR EACH STATEMENT EXECUTE FUNCTION organization_change_log(%s)', target, arguments);
END
$$;--> statement-breakpoint

-- Cloud-owned tables attach now; the Config Store attaches its tables when it opens.
SELECT organization_change_attach('organization', 'id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('environment_canvas_node_position', 'organization_id', 'resource_type', 'resource_id');
--> statement-breakpoint
SELECT organization_change_attach('organization_pairing', 'organization_id', 'organization_id');
--> statement-breakpoint
SELECT organization_change_attach('organization_cluster_domain', 'organization_id', 'organization_id');
--> statement-breakpoint
SELECT organization_change_attach('enrollment_allocation', 'organization_id', 'cluster_key');
--> statement-breakpoint
SELECT organization_change_attach('invitation', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('machine_enrollment_token', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('machine_remove_attempt', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('member', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('organization_billing_state', 'organization_id', 'organization_id');
--> statement-breakpoint
SELECT organization_change_attach('organization_machine', 'organization_id', 'machine_id');
