import { updatedAt } from "#/db/tables";

import { organization } from "#/modules/organization/tables";
import { user } from "#/modules/identity/tables";
import { environment, project, type SetupCommand } from "#/modules/project/tables";
import type { BranchPicks } from "#/modules/branches/branch-plan";
import type { IdentitySources } from "#/modules/branches/branch-operations.server";
import type { SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import type { JsonValue } from "#/db/tables";
import type { BranchHostnames, BranchOption, BranchPick, BranchRow } from "@ployz/sdk/config";

import { bigint, boolean, foreignKey, index, integer, jsonb, pgTable, primaryKey, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";

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

/** One approved row as the reviewer saw it: redacted, with its value choice, and whether its new value is still missing. */
export type HeldRow = { row: BranchRow; option?: BranchOption; missing: boolean };

/** A Conditional Save as the browser has it: no sealed values, and its approver's name. */
export type ConditionalSaveRow = {
  id: string; organizationId: string; projectId: string; prEnvironmentId: string | null; repositoryId: number; prNumber: number;
  destinationEnvironmentId: string; rows: HeldRow[]; workingRevision: string; targetBranch: string;
  approvedBy: string | null; approvedAt: Date;
};

/**
 * What landing needs without reading the PR Environment, which may be gone by then: core's comparison inputs but the
 * Destination's (`into`, `provided`), sealed values included, and the arriving nodes' identity sources.
 */
export type HeldLanding = {
  base: SavedEnvironmentIntent; from: SavedEnvironmentIntent; parent: SavedEnvironmentIntent | null;
  hostnames: BranchHostnames; identities: IdentitySources;
};

/**
 * A Conditional Save: a PR Environment's approved changes, held on one Destination until its pull request merges. It
 * stands while the PR Environment's revision and the pull request's target Git branch are what they were at approval.
 * `picks`, `approved_against` and `landing` hold sealed values and never reach the browser.
 */
export const conditionalSave = pgTable(
  "conditional_save",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    projectId: uuid("project_id").notNull(),
    // Goes with the PR Environment; landing reads `landing` instead.
    prEnvironmentId: uuid("pr_environment_id")
      .references(() => environment.id, { onDelete: "cascade" }),
    repositoryId: bigint("repository_id", { mode: "number" }).notNull(),
    prNumber: integer("pr_number").notNull(),
    destinationEnvironmentId: uuid("destination_environment_id")
      .notNull()
      .references(() => environment.id, { onDelete: "cascade" }),
    rows: jsonb("rows").notNull().$type<HeldRow[]>(),
    // Core's picks, new values sealed; a missing new value has none.
    picks: jsonb("picks").notNull().$type<BranchPick[]>(),
    // Each picked row's Destination value at approval, redacted: secrets by fingerprint.
    approvedAgainst: jsonb("approved_against").notNull().$type<Record<string, JsonValue>>(),
    landing: jsonb("landing").notNull().$type<HeldLanding>(),
    workingRevision: uuid("working_revision").notNull(),
    targetBranch: text("target_branch").notNull(),
    approvedByUserId: uuid("approved_by_user_id")
      .references(() => user.id, { onDelete: "set null" }),
    approvedAt: timestamp("approved_at", { withTimezone: true }).defaultNow().notNull(),
  },
  (table) => [
    uniqueIndex("conditional_save_pr_destination_idx").on(table.prEnvironmentId, table.destinationEnvironmentId),
    foreignKey({
      name: "conditional_save_project_fkey",
      columns: [table.organizationId, table.projectId],
      foreignColumns: [project.organizationId, project.id],
    }).onDelete("cascade"),
    index("conditional_save_organization_idx").on(table.organizationId),
    index("conditional_save_destination_idx").on(table.destinationEnvironmentId),
  ],
);
