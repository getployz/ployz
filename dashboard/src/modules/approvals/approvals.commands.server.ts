import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import { APPROVAL_STATUSES, approvalSubject, destroyedLine } from "#/modules/approvals/approvals";
import { type ApprovalView, decideApproval } from "#/modules/approvals/approvals.server";
import { defineCommand, type Ran } from "#/modules/commands/command.server";
import { freshApproval, freshOperationDigest, freshPendingApprovals } from "#/modules/machines/server-operations.server";

const ApprovalId = Schema.String.annotate({ description: "The approval id" });

const Waiting = Schema.Struct({
  id: Schema.String,
  status: Schema.Literals(APPROVAL_STATUSES),
  verb: Schema.String,
  name: Schema.String,
  created_at: Schema.String,
});

const Answered = Schema.Struct({
  id: Schema.String,
  status: Schema.Literals(APPROVAL_STATUSES),
  verb: Schema.String,
  name: Schema.String,
  destroys: Schema.Array(Schema.String),
  reason: Schema.NullOr(Schema.String),
  created_at: Schema.String,
  decided_at: Schema.NullOr(Schema.String),
});

const waiting = (approval: ApprovalView): typeof Waiting.Type => ({
  id: approval.id,
  status: approval.status,
  ...approvalSubject(approval.command, approval.review),
  created_at: approval.created_at,
});

const answered = (approval: ApprovalView): typeof Answered.Type => ({
  ...waiting(approval),
  destroys: approval.review.effects.map((effect) => {
    const { verb, name, after } = destroyedLine(effect, approval.command);
    return `${verb}${name === null ? "" : ` ${name}`}${after}`;
  }),
  reason: approval.reason,
  decided_at: approval.decided_at,
});

type Decided = { ok: true; approval: ApprovalView } | Extract<Ran<never>, { ok: false }>;

const told = (decided: Decided, said: (subject: string) => string): Ran<typeof Answered.Type> => {
  if (!decided.ok) return decided;
  const { verb, name } = approvalSubject(decided.approval.command, decided.approval.review);
  return { ok: true, output: answered(decided.approval), message: said(`${verb} ${name}`) };
};

export const orgApprovalLs = defineCommand({
  name: "org_approval_ls",
  path: ["org", "approval", "ls"],
  about: "List the Organization's pending approvals",
  examples: ["ployz org approval ls"],
  kind: "read",
  agents: true,
  typed: null,
  positional: [],
  input: Schema.Struct({}),
  output: Schema.Array(Waiting),
  render: "table",
  run: (caller) => freshPendingApprovals(caller.organization.id).pipe(
    Effect.map((approvals) => ({ ok: true, output: approvals.map(waiting) }) as const),
  ),
});

export const orgApprovalShow = defineCommand({
  name: "org_approval_show",
  path: ["org", "approval", "show"],
  about: "Show one approval: what the plan destroys and how it was answered",
  examples: ["ployz org approval show 7f3c2a10-5d2e-4b61-9c4e-2f0a1b3c4d5e"],
  kind: "read",
  agents: true,
  typed: null,
  positional: ["id"],
  input: Schema.Struct({ id: ApprovalId }),
  output: Answered,
  render: "record",
  run: (caller, { id }) => freshApproval(caller.organization.id, id).pipe(
    Effect.map((approval) => ({ ok: true, output: answered(approval) }) as const),
  ),
});

/**
 * Approves whatever the approval asks about now: a plan that changed since it was asked is superseded by the read, so
 * it refuses instead of approving something the human never saw.
 */
export const orgApprovalApprove = defineCommand({
  name: "org_approval_approve",
  path: ["org", "approval", "approve"],
  about: "Approve a pending approval so the command waiting on it goes ahead",
  examples: ["ployz org approval approve 7f3c2a10-5d2e-4b61-9c4e-2f0a1b3c4d5e"],
  kind: "write",
  removes: false,
  gate: "never",
  agents: false,
  typed: "organization",
  positional: ["id"],
  input: Schema.Struct({ id: ApprovalId }),
  output: Answered,
  render: "message",
  run: (caller, { id }) => Effect.gen(function* () {
    const { digest } = yield* freshApproval(caller.organization.id, id);
    const decided = yield* decideApproval(caller, id, { approve: { digest } }, freshOperationDigest);
    return told(decided, (subject) => `Approved: ${subject}. The command waiting on it goes ahead.`);
  }),
});

export const orgApprovalDeny = defineCommand({
  name: "org_approval_deny",
  path: ["org", "approval", "deny"],
  about: "Deny a pending approval; the agent or command waiting on it hears the reason",
  examples: ["ployz org approval deny 7f3c2a10-5d2e-4b61-9c4e-2f0a1b3c4d5e --reason \"not during the demo\""],
  kind: "write",
  removes: false,
  gate: "never",
  agents: false,
  typed: null,
  positional: ["id"],
  input: Schema.Struct({
    id: ApprovalId,
    reason: Schema.optionalKey(Schema.String.check(Schema.isMaxLength(1024)).annotate({ description: "Why not, passed back to whoever asked" })),
  }),
  output: Answered,
  render: "message",
  run: (caller, { id, reason }) => decideApproval(caller, id, { reject: reason === undefined ? {} : { reason } }, freshOperationDigest).pipe(
    Effect.map((decided) => told(decided, (subject) => `Denied: ${subject}.`)),
  ),
});
