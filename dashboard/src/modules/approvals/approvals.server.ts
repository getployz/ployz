import "@tanstack/react-start/server-only";
import { and, desc, eq, sql } from "drizzle-orm";
import { Effect, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import type { ApprovalDecision, ApprovalReview } from "#/modules/approvals/approvals";
import { withoutSealedBefore } from "#/modules/config-store/store-diff";
import { operationApprovals } from "#/modules/approvals/tables";
import type { StoreRefusal } from "#/lib/refusal";
import type { Actor, Caller } from "#/modules/identity/actor";
import type { Permission } from "#/modules/organization/permissions";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";

type ApprovalRow = typeof operationApprovals.$inferSelect;

const NO_SUCH_APPROVAL = "No such approval.";

/**
 * A review as anyone reads it: what runs once approved stays in Cloud, since a Sync's carries the secrets it sets. A
 * row stored before Cloud sealed its earlier values reads sealed too.
 */
const shownReview = ({ apply: _apply, ...review }: ApprovalReview, subject: string): ApprovalReview =>
  "operation" in review ? { ...review, operation: { ...review.operation, subject } } : { ...review, diff: withoutSealedBefore(review.diff) };

function approvalView(row: ApprovalRow) {
  return {
    id: row.id,
    status: row.status,
    digest: row.digest,
    command: row.command,
    reason: row.reason,
    review: shownReview(row.review, row.subject),
    protected: row.review.protected !== undefined,
    requested_by: row.requestedByUserId,
    created_at: row.createdAt.toISOString(),
    decided_at: row.decidedAt?.toISOString() ?? null,
  };
}
export type ApprovalView = ReturnType<typeof approvalView>;

const readRow = Effect.fn("Approvals.readRow")(function* (organizationId: string, id: string) {
  if (!Schema.is(Uuid)(id)) return undefined;
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select().from(operationApprovals)
    .where(and(eq(operationApprovals.id, id), eq(operationApprovals.organizationId, organizationId)));
  return row;
});

/**
 * A row as it stands now. Cloud keeps no approvals any more: a person says yes where they run the command, and the
 * resend carries the digest. A row still pending from before can never run anything, so it reads superseded, and an
 * older `ployz` that waits on it runs again and hears to update.
 */
const settle = Effect.fn("Approvals.settle")(function* (row: ApprovalRow) {
  if (row.status !== "pending") return row;
  const { drizzle } = yield* Database;
  const [superseded] = yield* drizzle.update(operationApprovals).set({ status: "superseded", updatedAt: sql`now()` })
    .where(and(eq(operationApprovals.id, row.id), eq(operationApprovals.status, "pending"))).returning();
  return superseded ?? (yield* readRow(row.organizationId, row.id)) ?? row;
});

/** One approval in the caller's Organization, as it stands now. */
export const getApproval = (organizationId: string, id: string) =>
  Effect.gen(function* () {
    const row = yield* readRow(organizationId, id);
    if (row === undefined) return yield* new NotFound({ message: NO_SUCH_APPROVAL });
    return approvalView(yield* settle(row));
  }).pipe(Effect.withSpan("Approvals.get"));

/** Every approval still waiting in the Organization: none, since each one read is settled first. */
export const pendingApprovals = (organizationId: string) =>
  Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const rows = yield* drizzle.select().from(operationApprovals)
      .where(and(eq(operationApprovals.organizationId, organizationId), eq(operationApprovals.status, "pending")))
      .orderBy(desc(operationApprovals.createdAt), desc(operationApprovals.id));
    const settled = yield* Effect.forEach(rows, settle);
    return settled.filter((row) => row.status === "pending").map(approvalView);
  }).pipe(Effect.withSpan("Approvals.pending"));

type Decided = { ok: true; approval: ApprovalView; decided?: true } | { ok: false; refusal: StoreRefusal };

/** Who answers: a person in the dashboard, or a Caller through the credential it signed in with. */
export type Decider = Actor & { readonly organization: { readonly id: string } } & (
  | { readonly via: "dashboard"; readonly permissions?: ReadonlyArray<Permission>; readonly environments?: ReadonlyArray<string> | undefined }
  | Pick<Caller, "via" | "credential" | "role" | "support">
);

/**
 * An answer to an approval: repeating the decision a row already holds answers the row, and anything else refuses
 * `conflict`. No row waits any more, so nothing new is decided and nothing runs.
 */
export const decideApproval = (caller: Decider, id: string, decision: ApprovalDecision) =>
  Effect.gen(function* () {
    const found = yield* readRow(caller.organization.id, id);
    if (found === undefined) return yield* new NotFound({ message: NO_SUCH_APPROVAL });
    if (caller.via === "agent") {
      return { ok: false, refusal: { code: "forbidden", message: "Agents can't answer approvals.", details: { approval: approvalView(found) } } } satisfies Decided;
    }
    const row = yield* settle(found);
    const repeated = "approve" in decision ? row.status === "approved" && row.digest === decision.approve.digest : row.status === "denied";
    if (repeated) return { ok: true, approval: approvalView(row) } satisfies Decided;
    const message = row.status === "superseded"
      ? "Nothing waits on this any more. Whoever asked runs the command again."
      : `This approval was already decided (${row.status}).`;
    return { ok: false, refusal: { code: "conflict", message, details: { approval: approvalView(row) } } } satisfies Decided;
  }).pipe(Effect.withSpan("Approvals.decide"));

/** `decideApproval` for a row that asked for a key: it never sends one, since no row waits. */
export const approveKeyed = (caller: Decider, input: { readonly id: string; readonly digest: string }) =>
  decideApproval(caller, input.id, { approve: { digest: input.digest } }).pipe(
    Effect.map((answer): Decided => answer.ok
      ? { ok: false, refusal: { code: "conflict", message: `This approval was already decided (${answer.approval.status}).`, details: { approval: answer.approval } } }
      : answer),
    Effect.withSpan("Approvals.approveKeyed"),
  );
