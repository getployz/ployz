import type { ModelMessage, RunRecord } from "@tanstack/ai";
import type { InterruptRecord } from "@tanstack/ai-persistence";
import { bigint, index, jsonb, pgTable, text, timestamp, uuid } from "drizzle-orm/pg-core";
import { createdAt, updatedAt } from "#/db/tables";
import { user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";

const owner = {
  organizationId: uuid("organization_id").notNull().references(() => organization.id, { onDelete: "cascade" }),
  userId: uuid("user_id").notNull().references(() => user.id, { onDelete: "cascade" }),
};

/** One member's sidebar conversation in one Organization. Its id is the client's `threadId`, owned by the first writer. */
export const agentThreads = pgTable("agent_threads", {
  threadId: text("thread_id").primaryKey(),
  ...owner,
  messages: jsonb("messages").notNull().$type<ModelMessage[]>(),
  createdAt,
  updatedAt,
}, (table) => [index("agent_threads_owner_idx").on(table.organizationId, table.userId)]);

/** One turn of a sidebar conversation, as TanStack AI records it. */
export const agentRuns = pgTable("agent_runs", {
  runId: text("run_id").primaryKey(),
  ...owner,
  threadId: text("thread_id").notNull(),
  status: text("status").notNull().$type<RunRecord["status"]>(),
  startedAt: bigint("started_at", { mode: "number" }).notNull(),
  record: jsonb("record").notNull().$type<RunRecord>(),
  createdAt,
  updatedAt,
}, (table) => [index("agent_runs_thread_idx").on(table.organizationId, table.userId, table.threadId)]);

/** A pause a run waits on, such as a gated Publish or Deploy's approval. */
export const agentInterrupts = pgTable("agent_interrupts", {
  interruptId: text("interrupt_id").primaryKey(),
  ...owner,
  runId: text("run_id").notNull(),
  threadId: text("thread_id").notNull(),
  status: text("status").notNull().$type<InterruptRecord["status"]>(),
  requestedAt: bigint("requested_at", { mode: "number" }).notNull(),
  record: jsonb("record").notNull().$type<InterruptRecord>(),
  /** The request resuming this interrupt, until it ends: one answer runs once however many clients resume it. */
  claim: text("claim"),
  claimedAt: timestamp("claimed_at", { withTimezone: true }),
  createdAt,
  updatedAt,
}, (table) => [
  index("agent_interrupts_thread_idx").on(table.organizationId, table.userId, table.threadId),
  index("agent_interrupts_run_idx").on(table.organizationId, table.userId, table.runId),
]);
