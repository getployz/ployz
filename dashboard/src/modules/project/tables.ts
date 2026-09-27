import { createdAt, updatedAt } from "#/db/tables";

import { organization } from "#/modules/organization/tables";
import type { SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";

import { foreignKey, jsonb, pgTable, text, unique, uuid, type AnyPgColumn } from "drizzle-orm/pg-core";



export const project = pgTable(
  "project",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    name: text("name").notNull(),
    slug: text("slug").notNull(),
    // Null once its Environment is torn down: the project then opens its oldest Environment.
    defaultEnvironmentId: uuid("default_environment_id")
      .references((): AnyPgColumn => environment.id, { onDelete: "set null" }),
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
