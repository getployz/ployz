import { sql } from "drizzle-orm";
import { bytea, check, index, integer, jsonb, pgTable, primaryKey, text, timestamp, uuid } from "drizzle-orm/pg-core";
import { sqlStringLiterals, updatedAt } from "#/db/tables";
import { organization } from "#/modules/organization/tables";

/**
 * One chunk of the source uploaded for one Deployment: a gzipped tar, in order. It is that Deployment's alone,
 * written once before it is admitted, and deleted once it ends.
 */
export const uploadChunk = pgTable(
  "upload_chunk",
  {
    deploymentId: text("deployment_id").notNull(),
    index: integer("index").notNull(),
    organizationId: uuid("organization_id").notNull().references(() => organization.id, { onDelete: "cascade" }),
    data: bytea("data").notNull(),
    createdAt: timestamp("created_at", { mode: "date", withTimezone: true }).notNull().defaultNow(),
  },
  (table) => [
    primaryKey({ columns: [table.deploymentId, table.index] }),
    index("upload_chunk_created_at_idx").on(table.createdAt),
    check("upload_chunk_index_check", sql`${table.index} >= 0`),
  ],
);

/**
 * One run of Cloud's worker for one Deployment, recorded before it does anything, so cancelling the run in Inngest
 * finds the Deployment to stop. It goes when the run ends.
 */
export const deploymentRun = pgTable("deployment_run", {
  runId: text("run_id").primaryKey(),
  organizationId: uuid("organization_id").notNull().references(() => organization.id, { onDelete: "cascade" }),
  deploymentId: text("deployment_id").notNull(),
  createdAt: timestamp("created_at", { mode: "date", withTimezone: true }).notNull().defaultNow(),
});

export const DEPLOYMENT_CLEANUP_STATES = ["waiting", "done", "left_running", "cancelled", "failed"] as const;
export type DeploymentCleanupState = (typeof DEPLOYMENT_CLEANUP_STATES)[number];

/**
 * An old copy a Deployment's cleanup could not retire. `machine` is the Machine's name. `service` is null for a Machine
 * the Deploy could not reach, whose copies it never saw.
 */
export type CleanupLeft = { machine: string; service: string | null; reason: string };

/**
 * How a Cloud Deploy's cleanup of the old copies it replaced went, for the Run page. A display record only: the Store
 * owns which copies are old. CLI Deploys and runs before cleanup was a step have none.
 */
export const deploymentCleanup = pgTable(
  "deployment_cleanup",
  {
    deploymentId: text("deployment_id").primaryKey(),
    organizationId: uuid("organization_id").notNull().references(() => organization.id, { onDelete: "cascade" }),
    environmentId: text("environment_id").notNull(),
    runId: text("run_id").notNull(),
    state: text("state").notNull().$type<DeploymentCleanupState>(),
    left: jsonb("left").notNull().default([]).$type<CleanupLeft[]>(),
    message: text("message"),
    updatedAt,
  },
  (table) => [
    check("deployment_cleanup_state_check", sql`${table.state} in (${sqlStringLiterals(DEPLOYMENT_CLEANUP_STATES)})`),
  ],
);

/** What the Run page shows of a Deployment's cleanup. */
export type DeploymentCleanupView = Pick<typeof deploymentCleanup.$inferSelect, "state" | "left" | "message">;
