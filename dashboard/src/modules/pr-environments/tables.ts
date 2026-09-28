import { updatedAt } from "#/db/tables";

import { organization } from "#/modules/organization/tables";
import { user } from "#/modules/identity/tables";
import { environment, environmentBranch, project, type SetupCommand } from "#/modules/project/tables";
import type { BranchPicks } from "#/modules/branches/branch-plan";
import type { IdentitySources } from "#/modules/branches/identity-sources";
import type { SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import type { BranchHostnames, BranchOption, BranchPick, BranchRow as ChangeRow } from "@ployz/sdk/config";

import { sql } from "drizzle-orm";
import { bigint, boolean, check, foreignKey, index, integer, jsonb, pgTable, primaryKey, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";

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

/**
 * A PR Environment: the Branch the system made for one pull request, whose facts each delivery refreshes from GitHub.
 * With "remove its environment" off it stays after the pull request closes, `closed`, and takes no more saves.
 * `retired` once it's being torn down while its pull request lives on: a new one can start beside it, and deliveries
 * leave it be.
 */
export const prEnvironment = pgTable(
  "pr_environment",
  {
    environmentId: uuid("environment_id").primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    projectId: uuid("project_id").notNull(),
    repositoryId: bigint("repository_id", { mode: "number" }).notNull(),
    number: integer("number").notNull(),
    title: text("title").notNull(),
    author: text("author").notNull(),
    headBranch: text("head_branch").notNull(),
    targetBranch: text("target_branch").notNull(),
    commits: integer("commits").notNull(),
    closed: boolean("closed").default(false).notNull(),
    retired: boolean("retired").default(false).notNull(),
  },
  (table) => [
    foreignKey({
      name: "pr_environment_branch_fkey",
      columns: [table.environmentId],
      foreignColumns: [environmentBranch.environmentId],
    }).onDelete("cascade"),
    // One current PR Environment per pull request in a project; a retired one can still be tearing down beside a new one.
    uniqueIndex("pr_environment_pull_request_idx").on(table.repositoryId, table.number, table.projectId).where(sql`not ${table.retired}`),
    index("pr_environment_organization_idx").on(table.organizationId),
  ],
);

export type PullRequest = typeof prEnvironment.$inferSelect;

/** A Branch row as the browser has it: a PR Environment's with its pull request. */
export type BranchRow = typeof environmentBranch.$inferSelect & { pullRequest: PullRequest | null };

/**
 * One saved row as the Save sheet showed it: redacted, with its value choice. Once landed, how it came in where the
 * Destination had changed it too: `staged` as an ordinary change to deploy, or only a `hint` beside the Destination's own
 * undeployed edit.
 */
export type HeldRow = { row: ChangeRow; option?: BranchOption; landed?: "staged" | "hint" };

export type ConditionalSaveState = (typeof conditionalSave.$inferSelect)["state"];

/** A Conditional Save as the browser has it: no sealed values. */
export type ConditionalSaveRow = {
  id: string; organizationId: string; projectId: string; prEnvironmentId: string | null; repositoryId: number; prNumber: number;
  destinationEnvironmentId: string; rows: HeldRow[]; workingRevision: string; targetBranch: string;
  savedAt: Date; state: ConditionalSaveState; landedSavedStateId: string | null;
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
 * A Conditional Save: a PR Environment's saved changes, waiting on one Destination to go live with its pull request's
 * merge commit. `standing`: it goes with the PR Environment, and stands while that one's revision and the pull request's
 * target Git branch are what they were when saved. `frozen` at the merge, with its merge commit: it has left the PR
 * Environment, so nothing done there withdraws it, and landing reads `landing` instead. `landed`: only the rows the
 * Destination had changed too remain, each `staged` or a `hint`, marked until the Destination's next Saved revision.
 * `picks` and `landing` hold sealed values and never reach the browser.
 */
export const conditionalSave = pgTable(
  "conditional_save",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    projectId: uuid("project_id").notNull(),
    state: text("state", { enum: ["standing", "frozen", "landed"] }).default("standing").notNull(),
    prEnvironmentId: uuid("pr_environment_id")
      .references(() => environment.id, { onDelete: "cascade" }),
    repositoryId: bigint("repository_id", { mode: "number" }).notNull(),
    prNumber: integer("pr_number").notNull(),
    destinationEnvironmentId: uuid("destination_environment_id")
      .notNull()
      .references(() => environment.id, { onDelete: "cascade" }),
    rows: jsonb("rows").notNull().$type<HeldRow[]>(),
    // Core's picks, new values sealed.
    picks: jsonb("picks").notNull().$type<BranchPick[]>(),
    landing: jsonb("landing").notNull().$type<HeldLanding>(),
    workingRevision: uuid("working_revision").notNull(),
    targetBranch: text("target_branch").notNull(),
    savedByUserId: uuid("saved_by_user_id")
      .references(() => user.id, { onDelete: "set null" }),
    savedAt: timestamp("saved_at", { withTimezone: true }).defaultNow().notNull(),
    mergeCommitSha: text("merge_commit_sha"),
    // The Saved revision landing published.
    landedSavedStateId: uuid("landed_saved_state_id"),
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
    check("conditional_save_state_check", sql`(
      ${table.state} = 'standing' and ${table.prEnvironmentId} is not null and ${table.mergeCommitSha} is null and ${table.landedSavedStateId} is null
    ) or (
      ${table.state} = 'frozen' and ${table.prEnvironmentId} is null and ${table.mergeCommitSha} is not null and ${table.landedSavedStateId} is null
    ) or (
      ${table.state} = 'landed' and ${table.prEnvironmentId} is null and ${table.mergeCommitSha} is not null and ${table.landedSavedStateId} is not null
    )`),
  ],
);
