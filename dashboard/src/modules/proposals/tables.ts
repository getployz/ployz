import type { Signed } from "@ployz/sdk";
import { sql } from "drizzle-orm";
import { check, index, jsonb, pgTable, text, timestamp, uuid } from "drizzle-orm/pg-core";
import { createdAt, sqlStringLiterals, updatedAt } from "#/db/tables";
import type { CredentialKind } from "#/modules/identity/actor";
import { user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { PROPOSAL_STATUSES, type ProposalApprovals, type ProposalHalt, type ProposalStatus, type ProposalStep } from "#/modules/proposals/proposal";

/**
 * An agent's ordered plan of restarts, stops and starts in one Environment, which a person approves with one passkey
 * prompt. Only its asker, through the credential that staged it, adds steps, and each step runs as them.
 */
export const operationProposals = pgTable(
  "operation_proposals",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    environmentId: text("environment_id").notNull(),
    project: text("project").notNull(),
    environment: text("environment").notNull(),
    namespace: text("namespace").notNull(),
    requestedByUserId: uuid("requested_by_user_id").references(() => user.id, { onDelete: "set null" }),
    credentialKind: text("credential_kind").notNull().$type<CredentialKind>(),
    credentialId: text("credential_id").notNull(),
    steps: jsonb("steps").default([]).notNull().$type<ProposalStep[]>(),
    status: text("status").default("draft").notNull().$type<ProposalStatus>(),
    /** The approver's signed bundle of every step's digest, in order. Public material only. */
    bundle: jsonb("bundle").$type<Signed | null>(),
    /** Each step's envelopes, by step then Container, each carrying the bundle. Public material only. */
    approvals: jsonb("approvals").$type<ProposalApprovals | null>(),
    halt: jsonb("halt").$type<ProposalHalt | null>(),
    reason: text("reason"),
    decidedByUserId: uuid("decided_by_user_id").references(() => user.id, { onDelete: "set null" }),
    decidedAt: timestamp("decided_at", { mode: "date", withTimezone: true }),
    createdAt,
    updatedAt,
  },
  (table) => [
    index("operation_proposals_waiting_idx").on(table.organizationId, table.status, table.createdAt.desc()),
    check("operation_proposals_status_check", sql`${table.status} in (${sqlStringLiterals(PROPOSAL_STATUSES)})`),
    check("operation_proposals_steps_check", sql`jsonb_typeof(${table.steps}) = 'array'`),
    check("operation_proposals_reason_check", sql`${table.reason} is null or length(${table.reason}) between 1 and 1024`),
    check("operation_proposals_decided_check", sql`(${table.status} in ('approved', 'denied')) = (${table.decidedAt} is not null)`),
  ],
);
