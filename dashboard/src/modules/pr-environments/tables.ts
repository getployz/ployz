import { updatedAt } from "#/db/tables";

import { organization } from "#/modules/organization/tables";
import { user } from "#/modules/identity/tables";
import { environment, project, type SetupCommand } from "#/modules/project/tables";
import type { BranchPicks } from "#/modules/branches/branch-plan";

import { bigint, boolean, foreignKey, index, jsonb, pgTable, primaryKey, text, uuid } from "drizzle-orm/pg-core";

/**
 * A project's PR Environments plan for one GitHub repository its services deploy from. No row means Off with the defaults.
 * Every PR Environment starts as a Branch of `start_from_environment_id`; null once that Environment is torn down, and then
 * none starts until someone picks another.
 */
export const prEnvironmentPlan = pgTable(
  "pr_environment_plan",
  {
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    projectId: uuid("project_id").notNull(),
    repositoryId: bigint("repository_id", { mode: "number" }).notNull(),
    installationId: bigint("installation_id", { mode: "number" }).notNull(),
    // The full name, for display: "acme/app".
    repository: text("repository").notNull(),
    enabled: boolean("enabled").default(false).notNull(),
    // Same project: checked on write, as the Default Environment is.
    startFromEnvironmentId: uuid("start_from_environment_id"),
    // By lineage, so they survive a new start-from Environment.
    picks: jsonb("picks").default({ preset: "only" }).notNull().$type<BranchPicks>(),
    setupCommands: jsonb("setup_commands").default([]).notNull().$type<SetupCommand[]>(),
    removeOnClose: boolean("remove_on_close").default(true).notNull(),
    includeBots: boolean("include_bots").default(false).notNull(),
    // Who PR Environments act as; set when they're turned on, cleared when off.
    enabledByUserId: uuid("enabled_by_user_id")
      .references(() => user.id, { onDelete: "set null" }),
    updatedAt,
  },
  (table) => [
    primaryKey({ columns: [table.projectId, table.repositoryId] }),
    foreignKey({
      name: "pr_environment_plan_start_from_fkey",
      columns: [table.startFromEnvironmentId],
      foreignColumns: [environment.id],
    }).onDelete("set null"),
    foreignKey({
      name: "pr_environment_plan_project_fkey",
      columns: [table.organizationId, table.projectId],
      foreignColumns: [project.organizationId, project.id],
    }).onDelete("cascade"),
    index("pr_environment_plan_organization_idx").on(table.organizationId),
  ],
);
