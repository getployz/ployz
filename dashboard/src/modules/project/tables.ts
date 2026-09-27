import { createdAt, updatedAt } from "#/db/tables";

import { organization } from "#/modules/organization/tables";
import { user } from "#/modules/identity/tables";
import type { SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";

import { sql } from "drizzle-orm";
import { bigint, boolean, check, foreignKey, index, integer, jsonb, pgTable, text, unique, uniqueIndex, uuid, type AnyPgColumn } from "drizzle-orm/pg-core";



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
    // Prefills every new Branch of this Environment; saved at once, never staged.
    branchSetupCommands: jsonb("branch_setup_commands").default([]).notNull().$type<SetupCommand[]>(),
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

/** A command a Branch runs in one Own Copy's image before that service first starts. */
export type SetupCommand = { lineageId: string; command: string };

/** One row per Branch; a root Environment has none. It goes with its Environment, and holds its Parent in place. */
export const environmentBranch = pgTable(
  "environment_branch",
  {
    environmentId: uuid("environment_id").primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    projectId: uuid("project_id").notNull(),
    // No action, not restrict: a project teardown deletes a Parent and its Branches in one statement.
    parentEnvironmentId: uuid("parent_environment_id").notNull(),
    kept: boolean("kept").default(false).notNull(),
    // Core's redacted, lineage-keyed configuration: sealed values are fingerprints only.
    base: jsonb("base").notNull().$type<SavedEnvironmentIntent>(),
    setupCommands: jsonb("setup_commands").default([]).notNull().$type<SetupCommand[]>(),
    createdByUserId: uuid("created_by_user_id")
      .notNull()
      .references(() => user.id, { onDelete: "restrict" }),
    // A PR Environment's pull request, kept current by its deliveries; all null on any other Branch.
    prRepositoryId: bigint("pr_repository_id", { mode: "number" }),
    // The repository's full name, for display: "acme/app".
    prRepository: text("pr_repository"),
    prNumber: integer("pr_number"),
    prTitle: text("pr_title"),
    prAuthor: text("pr_author"),
    prHeadBranch: text("pr_head_branch"),
    prHeadSha: text("pr_head_sha"),
    prTargetBranch: text("pr_target_branch"),
    createdAt,
  },
  (table) => [
    foreignKey({
      name: "environment_branch_environment_fkey",
      columns: [table.projectId, table.environmentId],
      foreignColumns: [environment.projectId, environment.id],
    }).onDelete("cascade"),
    foreignKey({
      name: "environment_branch_parent_fkey",
      columns: [table.projectId, table.parentEnvironmentId],
      foreignColumns: [environment.projectId, environment.id],
    }),
    check("environment_branch_not_own_parent", sql`${table.environmentId} <> ${table.parentEnvironmentId}`),
    check("environment_branch_pull_request", sql`num_nulls(${table.prRepositoryId}, ${table.prRepository}, ${table.prNumber}, ${table.prTitle}, ${table.prAuthor}, ${table.prHeadBranch}, ${table.prHeadSha}, ${table.prTargetBranch}) in (0, 8)`),
    // One PR Environment per pull request in a project.
    uniqueIndex("environment_branch_pull_request_idx").on(table.projectId, table.prRepositoryId, table.prNumber),
    index("environment_branch_organization_idx").on(table.organizationId),
    index("environment_branch_parent_idx").on(table.parentEnvironmentId),
  ],
);
