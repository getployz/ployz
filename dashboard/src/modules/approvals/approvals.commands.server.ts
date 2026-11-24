import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import { APPROVAL_STATUSES, approvalTarget, destroyedLines } from "#/modules/approvals/approvals";
import { type ApprovalView, decideApproval, getApproval, pendingApprovals } from "#/modules/approvals/approvals.server";
import { defineCommand, type Ran } from "#/modules/commands/define";

const ApprovalId = Schema.String.annotate({ description: "The Approval's ID, from a list of Approvals or from the call that had to ask" });

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

const waiting = (approval: ApprovalView): typeof Waiting.Type => {
  const { verb, name } = approvalTarget(approval.command, approval.review);
  return { id: approval.id, status: approval.status, verb, name, created_at: approval.created_at };
};

const answered = (approval: ApprovalView): typeof Answered.Type => ({
  ...waiting(approval),
  destroys: destroyedLines(approval.command, approval.review),
  reason: approval.reason,
  decided_at: approval.decided_at,
});

type Decided = { ok: true; approval: ApprovalView } | Extract<Ran<never>, { ok: false }>;

const told = (decided: Decided, said: (subject: string) => string): Ran<typeof Answered.Type> => {
  if (!decided.ok) return decided;
  const { verb, name } = approvalTarget(decided.approval.command, decided.approval.review);
  return { ok: true, output: answered(decided.approval), message: said(`${verb} ${name}`) };
};

export const orgApprovalLs = defineCommand({
  name: "org_approval_ls",
  requires: "approvals:answer",
  path: ["org", "approval", "ls"],
  about: "List the Approvals waiting on a human in the Organization. Each shows its ID, what it would do and to what, and when it was asked",
  title: "List pending approvals",
  examples: ["ployz org approval ls"],
  kind: "read",
  agents: true,
  openWorld: false,
  typed: null,
  positional: [],
  input: Schema.Struct({}),
  output: Schema.Array(Waiting),
  render: "table",
  run: (caller) => pendingApprovals(caller.organization.id).pipe(
    Effect.map((approvals) => ({ ok: true, output: approvals.map(waiting) }) as const),
  ),
});

export const orgApprovalShow = defineCommand({
  name: "org_approval_show",
  requires: "approvals:answer",
  path: ["org", "approval", "show"],
  about: "Show one Approval: what it would do and to what, what it destroys, whether it is pending, approved, denied or superseded, and the reason if it was denied",
  title: "Show an approval",
  examples: ["ployz org approval show 7f3c2a10-5d2e-4b61-9c4e-2f0a1b3c4d5e"],
  kind: "read",
  agents: true,
  openWorld: false,
  typed: null,
  positional: ["id"],
  input: Schema.Struct({ id: ApprovalId }),
  output: Answered,
  render: "record",
  run: (caller, { id }) => getApproval(caller.organization.id, id).pipe(
    Effect.map((approval) => ({ ok: true, output: answered(approval) }) as const),
  ),
});

/**
 * Approves whatever the approval asks about now: a plan that changed since it was asked is superseded by the read, so
 * it refuses instead of approving something the human never saw.
 */
export const orgApprovalApprove = defineCommand({
  name: "org_approval_approve",
  requires: "approvals:answer",
  path: ["org", "approval", "approve"],
  about: "Approve a pending Approval. The command waiting on it then goes ahead",
  title: "Approve a pending approval",
  examples: ["ployz org approval approve 7f3c2a10-5d2e-4b61-9c4e-2f0a1b3c4d5e"],
  kind: "write",
  removes: false,
  // Approving again answers the approved row; only the decision that approved it runs what waited.
  idempotent: true,
  gate: "never",
  agents: false,
  openWorld: false,
  typed: "organization",
  positional: ["id"],
  input: Schema.Struct({ id: ApprovalId }),
  output: Answered,
  render: "message",
  run: (caller, { id }) => Effect.gen(function* () {
    const { digest } = yield* getApproval(caller.organization.id, id);
    const decided = yield* decideApproval(caller, id, { approve: { digest } });
    return told(decided, (subject) => `Approved: ${subject}. The command waiting on it goes ahead.`);
  }),
});

export const orgApprovalDeny = defineCommand({
  name: "org_approval_deny",
  requires: "approvals:answer",
  path: ["org", "approval", "deny"],
  about: "Deny a pending Approval. The agent or command waiting on it is told the reason",
  title: "Deny a pending approval",
  examples: ["ployz org approval deny 7f3c2a10-5d2e-4b61-9c4e-2f0a1b3c4d5e --reason \"not during the demo\""],
  kind: "write",
  removes: false,
  // Denying again answers the denied row and keeps its first reason.
  idempotent: true,
  gate: "never",
  agents: false,
  openWorld: false,
  typed: null,
  positional: ["id"],
  input: Schema.Struct({
    id: ApprovalId,
    reason: Schema.optionalKey(Schema.String.check(Schema.isMaxLength(1024)).annotate({ description: "Why you are denying it, e.g. not during the demo. Whoever asked sees it" })),
  }),
  output: Answered,
  render: "message",
  run: (caller, { id, reason }) => decideApproval(caller, id, { reject: reason === undefined ? {} : { reason } }).pipe(
    Effect.map((decided) => told(decided, (subject) => `Denied: ${subject}.`)),
  ),
});
