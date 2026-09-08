import { createdAt, type EncryptedSecretValue, updatedAt } from "#/db/tables";

import { type EnvironmentResourceType } from "#/modules/environment-design/environment-resource-types";

import { organization } from "#/modules/organization/tables";

import { environment, project } from "#/modules/project/tables";

import { sql } from "drizzle-orm";

import { createSelectSchema } from "drizzle-orm/effect-schema";

import { boolean, check, integer, foreignKey, index, jsonb, pgTable, text, timestamp, unique, uniqueIndex, uuid } from "drizzle-orm/pg-core";



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
    | { kind: "secret"; encryptedValue: EncryptedSecretValue | null }
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

export type {
  ServiceGitBranch, ServiceImageAutoUpdate, ServiceImageCredentials,
  ServiceSource, ServiceHealthcheck, ServiceRestartPolicy, ServiceRoute,
  ServiceManagedHostname, ServiceBuilder, ServiceBuildConfig,
} from "@ployz/sdk/config";

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
    hasRegistryCredential: boolean("has_registry_credential").default(false).notNull(),
    firstDeployedAt: timestamp("first_deployed_at", {
      mode: "date",
      withTimezone: true,
    }),
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.projectId, table.id),
    unique().on(table.environmentId, table.id),
    unique().on(table.environmentId, table.lineageId),
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
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.projectId, table.id),
    unique().on(table.environmentId, table.id),
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
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.projectId, table.id),
    unique().on(table.environmentId, table.id),
    unique().on(table.environmentId, table.lineageId),
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

/** Opaque variable identity and tenant ownership; authored values live in environment.intent. */
export const variable = pgTable("variable", {
  id: uuid("id").defaultRandom().primaryKey(),
  environmentId: uuid("environment_id").notNull().references(() => environment.id, { onDelete: "cascade" }),
  serviceId: uuid("service_id").references(() => service.id, { onDelete: "cascade" }),
  variableGroupId: uuid("variable_group_id").references(() => environmentVariableGroup.id, { onDelete: "cascade" }),
  createdAt,
}, (table) => [
  unique().on(table.environmentId, table.id),
  check("variable_owner_check", sql`num_nonnulls(${table.serviceId}, ${table.variableGroupId}) = 1`),
  foreignKey({ columns: [table.environmentId, table.serviceId], foreignColumns: [service.environmentId, service.id] }).onDelete("cascade"),
  foreignKey({ columns: [table.environmentId, table.variableGroupId], foreignColumns: [environmentVariableGroup.environmentId, environmentVariableGroup.id] }).onDelete("cascade"),
]);

export const variableSecret = pgTable("variable_secret", {
  variableId: uuid("variable_id").primaryKey(),
  environmentId: uuid("environment_id").notNull().references(() => environment.id, { onDelete: "cascade" }),
  encryptedValue: jsonb("encrypted_value")
    .notNull()
    .$type<EncryptedSecretValue>(),
}, (table) => [
  foreignKey({ columns: [table.environmentId, table.variableId], foreignColumns: [variable.environmentId, variable.id] }).onDelete("cascade"),
]);

export const resourceLineageSelectSchema = createSelectSchema(resourceLineage);

export const environmentResourceSelectSchema =
  createSelectSchema(environmentResource);

export const environmentVariableGroupSelectSchema = createSelectSchema(
  environmentVariableGroup,
);
