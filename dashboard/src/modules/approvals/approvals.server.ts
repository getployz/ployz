import "@tanstack/react-start/server-only";
import type { Approval, ConfigCommand } from "@ployz/sdk";
import { and, eq, sql } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import {
  type ApprovalDecision,
  type ApprovalReview,
  DEFAULT_ORGANIZATION_SETTINGS,
  type SetOrganizationSettingsInput,
} from "#/modules/approvals/approvals";
import { operationApprovals, organizationSettings } from "#/modules/approvals/tables";
import { readStore } from "#/modules/config-store/config-store.server";
import { refusedWith } from "#/modules/config-store/store-sdk.server";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import type { Actor, Caller } from "#/modules/identity/actor";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";

type ApprovalRow = typeof operationApprovals.$inferSelect;

export const askBeforeDestructive = Effect.fn("Approvals.askBeforeDestructive")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ ask: organizationSettings.askBeforeDestructive }).from(organizationSettings)
    .where(eq(organizationSettings.organizationId, organizationId));
  return row?.ask ?? DEFAULT_ORGANIZATION_SETTINGS.askBeforeDestructive;
});

export const setOrganizationSettings = Effect.fn("Approvals.setOrganizationSettings")(function* (
  actor: Actor,
  { organizationSlug, ...settings }: typeof SetOrganizationSettingsInput.Type,
) {
  const { id: organizationId } = yield* requireInfrastructureOrganization(actor, organizationSlug);
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.insert(organizationSettings).values({ organizationId, ...settings })
    .onConflictDoUpdate({ target: organizationSettings.organizationId, set: { ...settings, updatedAt: new Date() } })
    .returning({ id: organizationSettings.organizationId, askBeforeDestructive: organizationSettings.askBeforeDestructive });
  return row ?? { id: organizationId, ...settings };
});

/** An approval as the CLI reads it. */
function approvalView(row: ApprovalRow) {
  return {
    id: row.id,
    status: row.status,
    digest: row.digest,
    command: row.command,
    reason: row.reason,
    review: row.review,
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
 * Run `record`, then supersede the Environment's pending approvals whose version the Store's `diff` moved past, or
 * all of them once the Store finds no such Environment. One transaction under a lock per Environment, so the sweep
 * sees only rows recorded before it read the Store; a version once past never comes back, so it never sweeps a
 * current plan. A Store that can't answer sweeps nothing.
 */
const recordAndSweep = <A, E, R>(
  organizationId: string,
  environment: { id: string; project: string; name: string },
  record: Effect.Effect<A, E, R>,
) => Effect.gen(function* () {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`select pg_advisory_xact_lock(hashtext(${`approvals:${organizationId}:${environment.id}`}))`);
    const recorded = yield* record;
    const current = yield* readStore(organizationId, {
      query: "diff",
      environment: { project: environment.project, environment: environment.name },
    }).pipe(
      Effect.map((diff) => diff.environment.id === environment.id ? diff.version : null),
      Effect.catchIf(refusedWith("not_found"), () => Effect.succeed(null)),
      Effect.option,
    );
    if (Option.isSome(current)) {
      yield* drizzle.update(operationApprovals)
        .set({ status: "superseded", updatedAt: new Date() })
        .where(and(
          eq(operationApprovals.organizationId, organizationId),
          eq(operationApprovals.environmentId, environment.id),
          eq(operationApprovals.status, "pending"),
          current.value === null ? undefined : sql`not starts_with(${operationApprovals.digest}, ${`${current.value}:`})`,
        ));
    }
    return recorded;
  }));
});

/** The row, superseded first if its Environment moved on. */
const freshen = Effect.fn("Approvals.freshen")(function* (row: ApprovalRow) {
  if (row.status !== "pending") return row;
  yield* recordAndSweep(row.organizationId, row.review.diff.environment, Effect.void);
  return (yield* readRow(row.organizationId, row.id)) ?? row;
});

type Trusted = { ok: true; approval: Approval } | { ok: false; refusal: StoreRefusal };

/**
 * What the Store is told about a human's approval of one CLI write, from the Organization's setting and the approval
 * the CLI retries with (`x-ployz-approval`). A denied approval refuses with its reason, so the agent hears why.
 */
export const trustedApproval = Effect.fn("Approvals.trusted")(function* (
  organizationId: string,
  approvalId: string | null,
): Effect.fn.Return<Trusted, never, Database> {
  if (!(yield* askBeforeDestructive(organizationId).pipe(Effect.orDie))) return { ok: true, approval: "not_required" };
  if (approvalId === null) return { ok: true, approval: "required" };
  const row = yield* readRow(organizationId, approvalId).pipe(Effect.orDie);
  if (row === undefined) {
    return {
      ok: false,
      refusal: { code: "invalid_argument", message: `No approval ${approvalId} in this Organization.`, details: { approval_id: approvalId } },
    };
  }
  switch (row.status) {
    case "approved":
      return { ok: true, approval: { approved: row.digest } };
    case "denied":
      return {
        ok: false,
        refusal: {
          code: "approval_denied",
          message: `A human denied approval ${row.id}${row.reason === null ? "." : `: ${row.reason}`}`,
          details: { approval: approvalView(row) },
        },
      };
    case "pending":
    case "superseded":
      return { ok: true, approval: "required" };
  }
});

const RefusedReview = Schema.Struct({
  approval: Schema.String,
  diff: Schema.Struct({
    version: Schema.String,
    environment: Schema.Struct({ id: Schema.String, project: Schema.String, name: Schema.String }),
  }),
});

/**
 * Record the Store's `approval_required` as one pending approval per digest and answer the refusal with its ID added
 * as `details.approval_id`. Asking again while it is pending answers the same row. Recording sweeps the Environment's
 * approvals for plans the Store moved past, this one too when it arrives late.
 */
export const requestApproval = Effect.fn("Approvals.request")(function* (
  caller: Caller,
  command: ConfigCommand,
  refused: StoreRefusal,
) {
  const decoded = Schema.decodeUnknownOption(RefusedReview)(refused.details);
  if (Option.isNone(decoded)) {
    yield* Effect.logWarning("An approval_required refusal named no review; nothing was recorded.");
    return refused;
  }
  const { approval: digest, diff } = decoded.value;
  // SAFETY: the Store words the refusal's `effects` and `diff` as `DestructiveEffect[]` and `DiffView`.
  const { effects, diff: review } = refused.details as ApprovalReview;
  const organizationId = caller.organization.id;
  const recorded = yield* recordAndSweep(organizationId, diff.environment, Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.insert(operationApprovals).values({
      organizationId,
      environmentId: diff.environment.id,
      requestedByUserId: caller.userId,
      credentialKind: caller.credential.kind,
      credentialId: caller.credential.id,
      command: command.command,
      review: { effects, diff: review },
      digest,
    }).onConflictDoUpdate({
      target: [operationApprovals.organizationId, operationApprovals.digest],
      targetWhere: sql`${operationApprovals.status} = 'pending'`,
      set: { updatedAt: new Date() },
    }).returning({ id: operationApprovals.id });
    return row ?? (yield* Effect.die("an upsert returned no row"));
  }));
  return { ...refused, details: { effects, approval: digest, diff: review, approval_id: recorded.id } };
});

/** One approval in the caller's Organization, superseded first if its Environment moved on. */
export const getApproval = Effect.fn("Approvals.get")(function* (organizationId: string, id: string) {
  const row = yield* readRow(organizationId, id);
  if (row === undefined) return yield* new NotFound({ message: "No such approval." });
  return approvalView(yield* freshen(row));
});

type Decided = { ok: true; approval: ApprovalView } | { ok: false; refusal: StoreRefusal };

/**
 * Approve exactly the digest the human saw, or deny with a reason. Repeating a decision answers the row; anything
 * else on a row no longer pending, or a digest that isn't the row's, refuses `conflict` with the row.
 */
export const decideApproval = Effect.fn("Approvals.decide")(function* (caller: Caller, id: string, decision: ApprovalDecision) {
  const found = yield* readRow(caller.organization.id, id);
  if (found === undefined) return yield* new NotFound({ message: "No such approval." });
  const conflict = (message: string, current: ApprovalRow): Decided =>
    ({ ok: false, refusal: { code: "conflict", message, details: { approval: approvalView(current) } } });
  const settled = (current: ApprovalRow): Decided => {
    if ("approve" in decision ? current.status === "approved" && current.digest === decision.approve.digest : current.status === "denied") {
      return { ok: true, approval: approvalView(current) };
    }
    return current.status === "superseded"
      ? conflict("The plan changed since this was asked; run the command again to review it.", current)
      : conflict(`This approval is already ${current.status}.`, current);
  };
  const row = yield* freshen(found);
  if (row.status !== "pending") return settled(row);
  if ("approve" in decision && decision.approve.digest !== row.digest) {
    return conflict("That digest isn't the one this approval asks about; review it again.", row);
  }
  const { drizzle } = yield* Database;
  const now = new Date();
  const decided = { decidedByUserId: caller.userId, decidedAt: now, updatedAt: now };
  const [updated] = yield* drizzle.update(operationApprovals)
    .set("approve" in decision
      ? { status: "approved", ...decided }
      : { status: "denied", reason: decision.reject.reason ?? null, ...decided })
    .where(and(eq(operationApprovals.id, row.id), eq(operationApprovals.status, "pending")))
    .returning();
  if (updated !== undefined) return { ok: true, approval: approvalView(updated) } satisfies Decided;
  const current = yield* readRow(caller.organization.id, id);
  return current === undefined ? yield* new NotFound({ message: "No such approval." }) : settled(current);
});
