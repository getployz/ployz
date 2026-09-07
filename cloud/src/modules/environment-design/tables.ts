import { createdAt, type EncryptedSecretValue, updatedAt } from "#/db/tables";

import { type EnvironmentResourceType } from "#/modules/environment-design/environment-resource-types";

import { organization } from "#/modules/organization/tables";

import { environment, project } from "#/modules/project/tables";

import { sql } from "drizzle-orm";

import { createSelectSchema } from "drizzle-orm/effect-schema";

import { boolean, check, doublePrecision, foreignKey, index, integer, jsonb, pgTable, primaryKey, text, timestamp, unique, uniqueIndex, uuid } from "drizzle-orm/pg-core";



export const SERVICE_SOURCE_TYPES = ["empty", "git", "image"] as const;

export type ServiceSourceType = (typeof SERVICE_SOURCE_TYPES)[number];

export const REGISTRY_CREDENTIAL_PROVIDERS = [
  "docker-hub",
  "ghcr",
  "gitlab",
  "quay",
  "aws-ecr-public",
  "gcp-artifact-registry",
  "mcr",
  "custom",
] as const;

export type RegistryCredentialProvider =
  (typeof REGISTRY_CREDENTIAL_PROVIDERS)[number];

export const REGISTRY_CREDENTIAL_AUTH_MODES = [
  "username-password",
  "token-only",
] as const;

export type RegistryCredentialAuthMode =
  (typeof REGISTRY_CREDENTIAL_AUTH_MODES)[number];

export const VARIABLE_VALUE_KINDS = ["plain", "sealed"] as const;

export type VariableValueKind = (typeof VARIABLE_VALUE_KINDS)[number];

/**
 * A plain variable value is a sequence of parts. A pure-literal value is a
 * single `text` part; a "template" is just a plain value that also has `ref`
 * parts. References are stored by the producer's stable `lineageId` (shared
 * across environments) so renames never break them; the UI
 * resolves `lineageId` -> current slug for display. A `self` ref resolves
 * within the owning service/variable group's own scope (its own vars + managed
 * exports).
 */
export type ValuePartRefOwner =
  | { scope: "self" }
  | { scope: "service" | "variable_group"; lineageId: string };

export type ValuePart =
  | { kind: "text"; value: string }
  | { kind: "ref"; owner: ValuePartRefOwner; key: string };

export type EnvironmentSnapshotVariableProducer = {
  ownerScope: "service" | "variable_group";
  ownerId: string;
  ownerLineageId: string;
  key: string;
  value:
    | { kind: "literal"; value: string }
    | { kind: "secret"; encryptedValue: EncryptedSecretValue }
    | { kind: "template"; parts: ValuePart[] };
};

export const CANVAS_NODE_TYPES = [
  "service",
  "variable_group",
  "volume",
] as const;

export type CanvasNodeType = (typeof CANVAS_NODE_TYPES)[number];

export const CONFIG_KEY_SCOPES = [
  "service_lineage",
  "variable_group_lineage",
] as const;

export type ConfigKeyScope = (typeof CONFIG_KEY_SCOPES)[number];

export type ServiceGitBranch =
  | {
      type: "connected";
      name: string;
    }
  | {
      type: "disconnected";
      previousName: string | null;
    };

export type ServiceImageAutoUpdate =
  | {
      type: "off";
    }
  | {
      type: "track-tag";
      tag: string;
    };

export type ServiceImageCredentials =
  | {
      type: "none";
    }
  | {
      type: "configured";
      revision?: string;
    };

export type ServiceSource =
  | {
      version: 1;
      type: "empty";
      rootDir: string;
    }
  | {
      version: 2;
      type: "git";
      repository: string;
      repositoryId: number;
      installationId: number;
      rootDir: string;
      branch: ServiceGitBranch;
      autoDeploy: boolean;
      waitForCi: boolean;
    }
  | {
      version: 1;
      type: "image";
      image: string;
      autoUpdate: ServiceImageAutoUpdate;
      credentials: ServiceImageCredentials;
    };

export type ServiceHealthcheck =
  | {
      type: "none";
    }
  | {
      type: "http";
      path: string;
      timeoutSeconds: number;
    };

export type ServiceRestartPolicy =
  | "unless-stopped"
  | "always"
  | "on-failure"
  | "no";

/** A public route: a hostname served by this service on a given container port. */
export type ServiceRoute = {
  /** Cloud-only stable identity; omitted from the Rust deployment projection. */
  id: string;
  hostname: string;
  targetPort: number;
};

/**
 * A managed auto public URL. The served hostname is `{prefix}.{cluster-lease-domain}`;
 * Cloud owns the prefix (defaults to the service's private DNS name). `targetPort`
 * null means "bind to the service's PORT env var" (resolved at deploy time).
 */
export type ServiceManagedHostname = {
  prefix: string;
  targetPort: number | null;
};

export type ServiceBuilder = "dockerfile" | "auto";

export type ServiceBuildConfig = {
  builder: ServiceBuilder;
  dockerfilePath: string | null;
  watchPaths: string[];
};

export type ServiceDeployEnvValue =
  | {
      kind: "literal";
      value: string;
    }
  | {
      kind: "secret";
      variableId?: string;
      encryptedValue?: EncryptedSecretValue;
      fingerprint: string;
      // Set when this value was produced by resolving a template that embedded
      // one or more secret references (the whole interpolated string is sealed).
      interpolated?: boolean;
    };

export type ServiceDeploymentConfig = {
  version: 2;
  name: string;
  source: ServiceSource;
  preDeployCommand: string | null;
  startCommand: string | null;
  healthcheck: ServiceHealthcheck;
  restartPolicy: ServiceRestartPolicy;
  env: Record<string, ServiceDeployEnvValue>;
};

export const serviceLineage = pgTable(
  "service_lineage",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    projectId: uuid("project_id")
      .notNull()
      .references(() => project.id, { onDelete: "cascade" }),
    canonicalName: text("canonical_name").notNull(),
    canonicalSlug: text("canonical_slug").notNull(),
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.projectId, table.canonicalSlug),
    unique().on(table.projectId, table.id),
    index("service_lineage_project_id_idx").on(table.projectId),
  ],
);

export const variableGroupLineage = pgTable(
  "variable_group_lineage",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    projectId: uuid("project_id")
      .notNull()
      .references(() => project.id, { onDelete: "cascade" }),
    canonicalName: text("canonical_name").notNull(),
    canonicalSlug: text("canonical_slug").notNull(),
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.projectId, table.canonicalSlug),
    unique().on(table.projectId, table.id),
    index("variable_group_lineage_project_id_idx").on(table.projectId),
  ],
);

export const resourceLineage = pgTable(
  "resource_lineage",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    projectId: uuid("project_id")
      .notNull()
      .references(() => project.id, { onDelete: "cascade" }),
    canonicalName: text("canonical_name").notNull(),
    canonicalSlug: text("canonical_slug").notNull(),
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.projectId, table.canonicalSlug),
    unique().on(table.projectId, table.id),
    index("resource_lineage_project_id_idx").on(table.projectId),
    index("resource_lineage_organization_id_idx").on(table.organizationId),
  ],
);

export const service = pgTable(
  "service",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    projectId: uuid("project_id")
      .notNull()
      .references(() => project.id, { onDelete: "cascade" }),
    environmentId: uuid("environment_id")
      .notNull()
      .references(() => environment.id, { onDelete: "cascade" }),
    lineageId: uuid("lineage_id")
      .notNull()
      .references(() => serviceLineage.id, { onDelete: "restrict" }),
    name: text("name").notNull(),
    slug: text("slug").notNull(),
    sourceType: text("source_type").notNull().$type<ServiceSourceType>(),
    sourceConfig: jsonb("source_config").notNull().$type<ServiceSource>(),
    preDeployCommand: text("pre_deploy_command"),
    startCommand: text("start_command"),
    healthcheck: jsonb("healthcheck")
      .default(sql`'{"type":"none"}'::jsonb`)
      .notNull()
      .$type<ServiceHealthcheck>(),
    restartPolicy: jsonb("restart_policy")
      .default(sql`'"unless-stopped"'::jsonb`)
      .notNull()
      .$type<ServiceRestartPolicy>(),
    maxRetries: integer("max_retries").default(10).notNull(),
    cron: text("cron"),
    replicas: integer("replicas").default(1).notNull(),
    cpuLimit: doublePrecision("cpu_limit"),
    memLimit: doublePrecision("mem_limit"),
    privateDns: text("private_dns").notNull(),
    routes: jsonb("routes")
      .default(sql`'[]'::jsonb`)
      .notNull()
      .$type<ServiceRoute[]>(),
    managedHostname: jsonb(
      "managed_hostname",
    ).$type<ServiceManagedHostname | null>(),
    build: jsonb("build")
      .default(
        sql`'{"builder":"auto","dockerfilePath":null,"watchPaths":[]}'::jsonb`,
      )
      .notNull()
      .$type<ServiceBuildConfig>(),
    hasRegistryCredential: boolean("has_registry_credential")
      .default(false)
      .notNull(),
    firstDeployedAt: timestamp("first_deployed_at", {
      mode: "date",
      withTimezone: true,
    }),
    deletedAt: timestamp("deleted_at", {
      mode: "date",
      withTimezone: true,
    }),
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.projectId, table.id),
    unique().on(table.environmentId, table.id),
    uniqueIndex("service_environment_slug_unique")
      .on(table.environmentId, table.slug)
      .where(sql`${table.deletedAt} is null`),
    uniqueIndex("service_environment_lineage_unique")
      .on(table.environmentId, table.lineageId)
      .where(sql`${table.deletedAt} is null`),
    index("service_project_id_idx").on(table.projectId),
    index("service_organization_id_idx").on(table.organizationId),
    index("service_environment_id_idx").on(table.environmentId),
    index("service_lineage_id_idx").on(table.lineageId),
    foreignKey({
      columns: [table.projectId, table.environmentId],
      foreignColumns: [environment.projectId, environment.id],
    }).onDelete("cascade"),
    foreignKey({
      columns: [table.projectId, table.lineageId],
      foreignColumns: [serviceLineage.projectId, serviceLineage.id],
    }).onDelete("restrict"),
  ],
);

export const serviceRegistryCredential = pgTable(
  "service_registry_credential",
  {
    serviceId: uuid("service_id")
      .primaryKey()
      .references(() => service.id, { onDelete: "cascade" }),
    encryptedRegistryUsername: jsonb("encrypted_registry_username").$type<
      EncryptedSecretValue | null
    >(),
    encryptedRegistrySecret: jsonb("encrypted_registry_secret").$type<
      EncryptedSecretValue | null
    >(),
  },
  (table) => [
    check(
      "service_registry_credential_nonempty_check",
      sql`num_nonnulls(${table.encryptedRegistryUsername}, ${table.encryptedRegistrySecret}) > 0`,
    ),
  ],
);

export const environmentCanvasNodePosition = pgTable(
  "environment_canvas_node_position",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    environmentId: uuid("environment_id")
      .notNull()
      .references(() => environment.id, { onDelete: "cascade" }),
    resourceType: text("resource_type").notNull(),
    resourceId: uuid("resource_id").notNull(),
    x: integer("x").notNull(),
    y: integer("y").notNull(),
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.environmentId, table.resourceType, table.resourceId),
    index("environment_canvas_node_position_environment_id_idx").on(
      table.environmentId,
    ),
    index("environment_canvas_node_position_organization_id_idx").on(
      table.organizationId,
    ),
    index("environment_canvas_node_position_resource_lookup_idx").on(
      table.resourceType,
      table.resourceId,
    ),
  ],
);

export const configKey = pgTable(
  "config_key",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    projectId: uuid("project_id")
      .notNull()
      .references(() => project.id, { onDelete: "cascade" }),
    scope: text("scope").notNull().$type<ConfigKeyScope>(),
    serviceLineageId: uuid("service_lineage_id").references(
      () => serviceLineage.id,
      { onDelete: "cascade" },
    ),
    variableGroupLineageId: uuid("variable_group_lineage_id").references(
      () => variableGroupLineage.id,
      { onDelete: "cascade" },
    ),
    canonicalName: text("canonical_name").notNull(),
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.projectId, table.id),
    uniqueIndex("config_key_service_lineage_canonical_name_unique")
      .on(table.projectId, table.serviceLineageId, table.canonicalName)
      .where(sql`${table.scope} = 'service_lineage'`),
    uniqueIndex("config_key_variable_group_lineage_canonical_name_unique")
      .on(table.projectId, table.variableGroupLineageId, table.canonicalName)
      .where(sql`${table.scope} = 'variable_group_lineage'`),
    index("config_key_project_id_idx").on(table.projectId),
    index("config_key_service_lineage_id_idx").on(table.serviceLineageId),
    index("config_key_variable_group_lineage_id_idx").on(
      table.variableGroupLineageId,
    ),
    foreignKey({
      columns: [table.projectId, table.serviceLineageId],
      foreignColumns: [serviceLineage.projectId, serviceLineage.id],
    }).onDelete("cascade"),
    foreignKey({
      columns: [table.projectId, table.variableGroupLineageId],
      foreignColumns: [variableGroupLineage.projectId, variableGroupLineage.id],
    }).onDelete("cascade"),
    check(
      "config_key_scope_owner_check",
      sql`(
        (${table.scope} = 'service_lineage' and ${table.serviceLineageId} is not null and ${table.variableGroupLineageId} is null) or
        (${table.scope} = 'variable_group_lineage' and ${table.serviceLineageId} is null and ${table.variableGroupLineageId} is not null)
      )`,
    ),
  ],
);

export const environmentVariableGroup = pgTable(
  "environment_variable_group",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    projectId: uuid("project_id")
      .notNull()
      .references(() => project.id, { onDelete: "cascade" }),
    environmentId: uuid("environment_id")
      .notNull()
      .references(() => environment.id, { onDelete: "cascade" }),
    lineageId: uuid("lineage_id")
      .notNull()
      .references(() => variableGroupLineage.id, { onDelete: "restrict" }),
    name: text("name").notNull(),
    slug: text("slug").notNull(),
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.projectId, table.id),
    unique().on(table.environmentId, table.slug),
    unique().on(table.environmentId, table.lineageId),
    index("environment_variable_group_project_id_idx").on(table.projectId),
    index("environment_variable_group_organization_id_idx").on(
      table.organizationId,
    ),
    index("environment_variable_group_environment_id_idx").on(
      table.environmentId,
    ),
    index("environment_variable_group_lineage_id_idx").on(table.lineageId),
    foreignKey({
      columns: [table.projectId, table.environmentId],
      foreignColumns: [environment.projectId, environment.id],
    }).onDelete("cascade"),
    foreignKey({
      columns: [table.projectId, table.lineageId],
      foreignColumns: [variableGroupLineage.projectId, variableGroupLineage.id],
    }).onDelete("restrict"),
  ],
);

export const environmentResource = pgTable(
  "environment_resource",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    projectId: uuid("project_id")
      .notNull()
      .references(() => project.id, { onDelete: "cascade" }),
    environmentId: uuid("environment_id")
      .notNull()
      .references(() => environment.id, { onDelete: "cascade" }),
    lineageId: uuid("lineage_id")
      .notNull()
      .references(() => resourceLineage.id, { onDelete: "restrict" }),
    implementationType: text("implementation_type")
      .notNull()
      .$type<EnvironmentResourceType>(),
    variableGroupId: uuid("variable_group_id").references(
      () => environmentVariableGroup.id,
      { onDelete: "restrict" },
    ),
    name: text("name").notNull(),
    slug: text("slug").notNull(),
    deletedAt: timestamp("deleted_at", {
      mode: "date",
      withTimezone: true,
    }),
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.projectId, table.id),
    unique().on(table.environmentId, table.id),
    uniqueIndex("environment_resource_environment_slug_unique")
      .on(table.environmentId, table.slug)
      .where(sql`${table.deletedAt} is null`),
    uniqueIndex("environment_resource_environment_lineage_unique")
      .on(table.environmentId, table.lineageId)
      .where(sql`${table.deletedAt} is null`),
    uniqueIndex("environment_resource_variable_group_variable_group_unique")
      .on(table.variableGroupId)
      .where(sql`${table.implementationType} = 'variable_group'`),
    index("environment_resource_project_id_idx").on(table.projectId),
    index("environment_resource_organization_id_idx").on(table.organizationId),
    index("environment_resource_environment_id_idx").on(table.environmentId),
    index("environment_resource_lineage_id_idx").on(table.lineageId),
    index("environment_resource_variable_group_id_idx").on(
      table.variableGroupId,
    ),
    foreignKey({
      columns: [table.projectId, table.environmentId],
      foreignColumns: [environment.projectId, environment.id],
    }).onDelete("cascade"),
    foreignKey({
      columns: [table.projectId, table.lineageId],
      foreignColumns: [resourceLineage.projectId, resourceLineage.id],
    }).onDelete("restrict"),
    foreignKey({
      columns: [table.projectId, table.variableGroupId],
      foreignColumns: [
        environmentVariableGroup.projectId,
        environmentVariableGroup.id,
      ],
    }).onDelete("restrict"),
    check(
      "environment_resource_implementation_type_check",
      sql`${table.implementationType} in ('variable_group', 'volume')`,
    ),
    check(
      "environment_resource_variable_group_reference_check",
      sql`(${table.implementationType} != 'variable_group' or ${table.variableGroupId} is not null)`,
    ),
    check(
      "environment_resource_volume_no_variable_group_check",
      sql`(${table.implementationType} != 'volume' or ${table.variableGroupId} is null)`,
    ),
  ],
);

export const variable = pgTable(
  "variable",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    projectId: uuid("project_id")
      .notNull()
      .references(() => project.id, { onDelete: "cascade" }),
    serviceId: uuid("service_id").references(() => service.id, {
      onDelete: "cascade",
    }),
    variableGroupId: uuid("variable_group_id").references(
      () => environmentVariableGroup.id,
      { onDelete: "cascade" },
    ),
    configKeyId: uuid("config_key_id")
      .notNull()
      .references(() => configKey.id, { onDelete: "restrict" }),
    key: text("key").notNull(),
    description: text("description"),
    exported: boolean("exported").default(false).notNull(),
    valueKind: text("value_kind").notNull().$type<VariableValueKind>(),
    // Canonical structured form of a plain value (null for sealed). A pure
    // literal is `[{ kind: "text", value }]`; refs embed `${{ }}`-style links.
    valueParts: jsonb("value_parts").$type<ValuePart[] | null>(),
    valueFingerprint: text("value_fingerprint").notNull(),
    createdAt,
    updatedAt,
  },
  (table) => [
    index("variable_project_id_idx").on(table.projectId),
    index("variable_organization_id_idx").on(table.organizationId),
    index("variable_service_id_idx").on(table.serviceId),
    index("variable_variable_group_id_idx").on(table.variableGroupId),
    index("variable_config_key_id_idx").on(table.configKeyId),
    index("variable_exported_idx").on(table.exported),
    unique().on(table.serviceId, table.key),
    unique().on(table.variableGroupId, table.key),
    uniqueIndex("variable_service_config_key_unique")
      .on(table.serviceId, table.configKeyId)
      .where(sql`${table.serviceId} is not null`),
    uniqueIndex("variable_group_config_key_unique")
      .on(table.variableGroupId, table.configKeyId)
      .where(sql`${table.variableGroupId} is not null`),
    check(
      "variable_owner_check",
      sql`num_nonnulls(${table.serviceId}, ${table.variableGroupId}) = 1`,
    ),
    foreignKey({
      columns: [table.projectId, table.configKeyId],
      foreignColumns: [configKey.projectId, configKey.id],
    }).onDelete("restrict"),
    foreignKey({
      columns: [table.projectId, table.serviceId],
      foreignColumns: [service.projectId, service.id],
    }).onDelete("cascade"),
    foreignKey({
      columns: [table.projectId, table.variableGroupId],
      foreignColumns: [
        environmentVariableGroup.projectId,
        environmentVariableGroup.id,
      ],
    }).onDelete("cascade"),
    check(
      "variable_value_kind_check",
      sql`(
        (${table.valueKind} = 'plain' and ${table.valueParts} is not null) or
        (${table.valueKind} = 'sealed' and ${table.valueParts} is null)
      )`,
    ),
  ],
);

export const variableSecret = pgTable("variable_secret", {
  variableId: uuid("variable_id")
    .primaryKey()
    .references(() => variable.id, { onDelete: "cascade" }),
  encryptedValue: jsonb("encrypted_value")
    .notNull()
    .$type<EncryptedSecretValue>(),
});

export const configValue = pgTable(
  "config_value",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    projectId: uuid("project_id")
      .notNull()
      .references(() => project.id, { onDelete: "cascade" }),
    configKeyId: uuid("config_key_id")
      .notNull()
      .references(() => configKey.id, { onDelete: "cascade" }),
    environmentId: uuid("environment_id")
      .notNull()
      .references(() => environment.id, { onDelete: "cascade" }),
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.configKeyId, table.environmentId),
    index("config_value_project_id_idx").on(table.projectId),
    index("config_value_environment_id_idx").on(table.environmentId),
    foreignKey({
      columns: [table.projectId, table.configKeyId],
      foreignColumns: [configKey.projectId, configKey.id],
    }).onDelete("cascade"),
    foreignKey({
      columns: [table.projectId, table.environmentId],
      foreignColumns: [environment.projectId, environment.id],
    }).onDelete("cascade"),
  ],
);

export const serviceVariableGroupAttachment = pgTable(
  "service_variable_group_attachment",
  {
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    serviceId: uuid("service_id")
      .notNull()
      .references(() => service.id, { onDelete: "cascade" }),
    variableGroupId: uuid("variable_group_id")
      .notNull()
      .references(() => environmentVariableGroup.id, { onDelete: "cascade" }),
    sortOrder: integer("sort_order").default(0).notNull(),
    createdAt,
  },
  (table) => [
    primaryKey({
      columns: [table.serviceId, table.variableGroupId],
    }),
    index("service_variable_group_attachment_service_id_idx").on(
      table.serviceId,
    ),
    index("service_variable_group_attachment_organization_id_idx").on(
      table.organizationId,
    ),
    index("service_variable_group_attachment_set_id_idx").on(
      table.variableGroupId,
    ),
  ],
);

export const serviceVolumeAttachment = pgTable(
  "service_volume_attachment",
  {
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    projectId: uuid("project_id")
      .notNull()
      .references(() => project.id, { onDelete: "cascade" }),
    environmentId: uuid("environment_id")
      .notNull()
      .references(() => environment.id, { onDelete: "cascade" }),
    serviceId: uuid("service_id").notNull(),
    volumeResourceId: uuid("volume_resource_id").notNull(),
    mountPath: text("mount_path").notNull(),
    createdAt,
    updatedAt,
  },
  (table) => [
    // One mount per service/volume pair (R9); a service can't mount two volumes
    // at the same path (R10).
    primaryKey({ columns: [table.serviceId, table.volumeResourceId] }),
    unique().on(table.serviceId, table.mountPath),
    index("service_volume_attachment_service_id_idx").on(table.serviceId),
    index("service_volume_attachment_volume_resource_id_idx").on(
      table.volumeResourceId,
    ),
    index("service_volume_attachment_project_id_idx").on(table.projectId),
    index("service_volume_attachment_organization_id_idx").on(
      table.organizationId,
    ),
    index("service_volume_attachment_environment_id_idx").on(
      table.environmentId,
    ),
    // Env-scoped composite FKs reject cross-environment mounts at the DB level.
    foreignKey({
      columns: [table.environmentId, table.serviceId],
      foreignColumns: [service.environmentId, service.id],
    }).onDelete("cascade"),
    foreignKey({
      columns: [table.environmentId, table.volumeResourceId],
      foreignColumns: [
        environmentResource.environmentId,
        environmentResource.id,
      ],
    }).onDelete("cascade"),
  ],
);

export const resourceLineageSelectSchema = createSelectSchema(resourceLineage);

export const environmentResourceSelectSchema =
  createSelectSchema(environmentResource);

export const environmentVariableGroupSelectSchema = createSelectSchema(
  environmentVariableGroup,
);

export const variableSelectSchema = createSelectSchema(variable);
