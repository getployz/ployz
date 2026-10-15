import { sql } from "drizzle-orm";
import { check, index, jsonb, pgTable, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";
import { createdAt, sqlStringLiterals, updatedAt } from "#/db/tables";
import type { CredentialKind } from "#/modules/identity/actor";
import { user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { APPROVAL_STATUSES, type ApprovalReview, type ApprovalStatus } from "#/modules/approvals/approvals";

/** One destructive CLI write or Server operation a human must approve, by the digest of exactly what it destroys. */
export const operationApprovals = pgTable(
  "operation_approvals",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    /** What the approval is about: an Environment ID, `server:<machine id>`, or `namespace:<name>`. */
    subject: text("subject").notNull(),
    requestedByUserId: uuid("requested_by_user_id").references(() => user.id, { onDelete: "set null" }),
    credentialKind: text("credential_kind").notNull().$type<CredentialKind>(),
    credentialId: text("credential_id").notNull(),
    /** The Store command refused, such as `publish` or `admit`, or the operation, such as `drain` or `clean`. */
    command: text("command").notNull(),
    review: jsonb("review").notNull().$type<ApprovalReview>(),
    /** `version:hash` as the Store words it, or `verb:hash` of an operation's preview. */
    digest: text("digest").notNull(),
    status: text("status").default("pending").notNull().$type<ApprovalStatus>(),
    reason: text("reason"),
    decidedByUserId: uuid("decided_by_user_id").references(() => user.id, { onDelete: "set null" }),
    decidedAt: timestamp("decided_at", { mode: "date", withTimezone: true }),
    createdAt,
    updatedAt,
  },
  (table) => [
    uniqueIndex("operation_approvals_one_pending_idx")
      .on(table.organizationId, table.digest)
      .where(sql`${table.status} = 'pending'`),
    index("operation_approvals_subject_idx").on(table.organizationId, table.subject, table.status),
    check("operation_approvals_status_check", sql`${table.status} in (${sqlStringLiterals(APPROVAL_STATUSES)})`),
    check("operation_approvals_credential_kind_check", sql`${table.credentialKind} in ('session', 'token', 'oauth')`),
    check("operation_approvals_reason_check", sql`${table.reason} is null or ${table.status} = 'denied'`),
    check("operation_approvals_decided_check", sql`(${table.status} in ('approved', 'denied')) = (${table.decidedAt} is not null)`),
  ],
);
