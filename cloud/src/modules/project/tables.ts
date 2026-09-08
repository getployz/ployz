import { createdAt, updatedAt } from "#/db/tables";

import { user } from "#/modules/identity/tables";

import { organization } from "#/modules/organization/tables";
import type { SavedEnvironmentIntent } from "@ployz/sdk/config";

import { foreignKey, index, jsonb, pgTable, text, unique, uuid } from "drizzle-orm/pg-core";



export const project = pgTable(
  "project",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    name: text("name").notNull(),
    slug: text("slug").notNull(),
    createdAt,
  },
  (table) => [
    unique().on(table.organizationId, table.slug),
    unique().on(table.organizationId, table.id),
  ],
);

export const environment = pgTable(
  "environment",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    projectId: uuid("project_id")
      .notNull()
      .references(() => project.id, { onDelete: "cascade" }),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    name: text("name").notNull(),
    namespace: text("namespace").notNull(),
    intent: jsonb("intent").notNull().$type<SavedEnvironmentIntent>(),
    revision: uuid("revision").defaultRandom().notNull(),
    updatedAt,
    createdAt,
  },
  (table) => [
    unique().on(table.projectId, table.name),
    unique().on(table.projectId, table.id),
    unique().on(table.organizationId, table.namespace),
    foreignKey({
      columns: [table.organizationId, table.projectId],
      foreignColumns: [project.organizationId, project.id],
    }).onDelete("cascade"),
  ],
);

export const userProjectPreference = pgTable(
  "user_project_preference",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    userId: uuid("user_id")
      .notNull()
      .references(() => user.id, { onDelete: "cascade" }),
    projectId: uuid("project_id")
      .notNull()
      .references(() => project.id, { onDelete: "cascade" }),
    environmentId: uuid("environment_id")
      .notNull()
      .references(() => environment.id, { onDelete: "cascade" }),
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.userId, table.projectId),
    index("user_project_preference_user_idx").on(table.userId),
    index("user_project_preference_organization_idx").on(table.organizationId),
    index("user_project_preference_project_idx").on(table.projectId),
  ],
);
