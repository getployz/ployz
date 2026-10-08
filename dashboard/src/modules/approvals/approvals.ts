import type { ConfigCommand, DestructiveEffect, DiffView, JsonValue } from "@ployz/sdk";
import { Schema } from "effect";

export const APPROVAL_STATUSES = ["pending", "approved", "denied", "superseded"] as const;
export type ApprovalStatus = (typeof APPROVAL_STATUSES)[number];

/** The Server and Namespace operations the CLI runs through Cloud that may wait for a human. Upgrade never does. */
export type OperationVerb = "drain" | "clean" | "remove";

/** An operation as a human approves it: its verb, what it acts on by name, and the preview its digest covers. */
export type OperationReview = { verb: OperationVerb; name: string; preview: JsonValue };

/**
 * What a human approves: the Destructive Effects, and either the Review they come from as the Store refused it, or the
 * operation's preview.
 */
export type ApprovalReview =
  | { effects: DestructiveEffect[]; diff: DiffView }
  | { effects: DestructiveEffect[]; operation: OperationReview };

/** The Organization's settings row as the Org Store reads it. No row reads as the defaults. */
export type OrganizationSettingsRow = { id: string; askBeforeDestructive: boolean };

/** An Organization without a settings row asks before destructive actions. */
export const DEFAULT_ORGANIZATION_SETTINGS = { askBeforeDestructive: true } satisfies Omit<OrganizationSettingsRow, "id">;

export function organizationSettings(rows: readonly OrganizationSettingsRow[]) {
  return rows[0] ?? DEFAULT_ORGANIZATION_SETTINGS;
}

export const SetOrganizationSettingsInput = Schema.Struct({
  organizationSlug: Schema.String,
  askBeforeDestructive: Schema.Boolean,
});

/** The writes the Store reviews for destruction: Publish, and a manual Deploy. Nothing else waits for a human. */
export function asksApproval(command: ConfigCommand) {
  return command.command === "publish" || (command.command === "admit" && command.admit === "deploy");
}

/** `POST /api/cli/approvals/:id`: approve exactly the digest the human saw, or deny with a reason the agent gets back. */
export const ApprovalDecision = Schema.Union([
  Schema.Struct({ approve: Schema.Struct({ digest: Schema.String.check(Schema.isNonEmpty()) }) }),
  Schema.Struct({ reject: Schema.Struct({ reason: Schema.optional(Schema.String.check(Schema.isMaxLength(1024))) }) }),
]);
export type ApprovalDecision = typeof ApprovalDecision.Type;
