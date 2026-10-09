import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { Approval, ConfigCommand, DestructiveEffect, JsonValue } from "@ployz/sdk";
import { and, desc, eq, ne, type SQL, sql } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import {
  type ApprovalDecision,
  type ApprovalReview,
  approvalSubject,
  DEFAULT_ORGANIZATION_SETTINGS,
  type OperationVerb,
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

type EnvironmentAsked = { id: string; project: string; name: string };

const versionById = Effect.fn("Approvals.versionById")(function* (organizationId: string, environment: EnvironmentAsked) {
  const versionAt = (project: string, name: string) =>
    readStore(organizationId, { query: "diff", environment: { project, environment: name } }).pipe(
      Effect.map((diff) => diff.environment.id === environment.id ? diff.version : null),
      Effect.catchIf(refusedWith("not_found"), () => Effect.succeed(null)),
    );
  const named = yield* versionAt(environment.project, environment.name);
  if (named !== null) return named;
  const { projects } = yield* readStore(organizationId, { query: "projects" });
  for (const project of projects) {
    const { environments } = yield* readStore(organizationId, { query: "environments", project: project.name });
    const found = environments.find(({ id }) => id === environment.id);
    if (found !== undefined) return yield* versionAt(project.name, found.name);
  }
  return null;
});

type Stale<R> = Effect.Effect<SQL | undefined | null, never, R>;

const recordAndSweep = <A, E, R, R2>(
  organizationId: string,
  subject: string,
  record: Effect.Effect<A, E, R>,
  stale: Stale<R2>,
) => Effect.gen(function* () {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`select pg_advisory_xact_lock(hashtext(${`approvals:${organizationId}:${subject}`}))`);
    const recorded = yield* record;
    const condition = yield* stale;
    if (condition !== null) {
      yield* drizzle.update(operationApprovals)
        .set({ status: "superseded", updatedAt: new Date() })
        .where(and(
          eq(operationApprovals.organizationId, organizationId),
          eq(operationApprovals.subject, subject),
          eq(operationApprovals.status, "pending"),
          condition,
        ));
    }
    return recorded;
  }));
});

const staleEnvironment = (organizationId: string, environment: EnvironmentAsked) =>
  versionById(organizationId, environment).pipe(
    Effect.map((version) => version === null ? undefined : sql`not starts_with(${operationApprovals.digest}, ${`${version}:`})`),
    Effect.orElseSucceed(() => null),
  );

const staleOperation = (verb: OperationVerb, fresh: string | null) => Effect.succeed(and(
  sql`starts_with(${operationApprovals.digest}, ${`${verb}:`})`,
  fresh === null ? undefined : ne(operationApprovals.digest, fresh),
));

export type OperationDigest<R> = (
  organizationId: string,
  subject: string,
  verb: OperationVerb,
  stored: string,
) => Effect.Effect<string | null, never, R>;

const freshen = <R>(row: ApprovalRow, operationDigest?: OperationDigest<R>) => Effect.gen(function* () {
  if (row.status !== "pending") return row;
  const review = row.review;
  if ("diff" in review) {
    yield* recordAndSweep(row.organizationId, row.subject, Effect.void, staleEnvironment(row.organizationId, review.diff.environment));
  } else if (operationDigest !== undefined) {
    const fresh = yield* operationDigest(row.organizationId, row.subject, review.operation.verb, row.digest);
    yield* recordAndSweep(row.organizationId, row.subject, Effect.void, staleOperation(review.operation.verb, fresh));
  }
  return (yield* readRow(row.organizationId, row.id)) ?? row;
});

export function operationDigest({ verb, preview, effects }: Pick<OperationAsked, "verb" | "preview" | "effects">) {
  return `${verb}:${createHash("sha256").update(canonicalJson({ preview, effects })).digest("hex")}`;
}

function canonicalJson(value: JsonValue): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value instanceof Object) {
    const entries = Object.entries(value).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
    return `{${entries.map(([key, entry]) => `${JSON.stringify(key)}:${canonicalJson(entry)}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

/** "the request to deploy production": a denial names what was asked, never the approval's ID. */
const denied = (row: ApprovalRow) => {
  const { verb, name } = approvalSubject(row.command, row.review);
  return `the request to ${verb.toLowerCase()} ${name}`;
};

type Trusted = { ok: true; approval: Approval } | { ok: false; refusal: StoreRefusal };

export const trustedApproval = Effect.fn("Approvals.trusted")(function* (
  organizationId: string,
  approvalId: string | null,
): Effect.fn.Return<Trusted, never, Database> {
  if (approvalId === null) {
    return { ok: true, approval: (yield* askBeforeDestructive(organizationId).pipe(Effect.orDie)) ? "required" : "not_required" };
  }
  const row = yield* readRow(organizationId, approvalId).pipe(Effect.orDie);
  if (row === undefined) {
    return {
      ok: false,
      refusal: { code: "invalid_argument", message: "No such approval in this Organization.", details: { approval_id: approvalId } },
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
          message: `A human denied ${denied(row)}${row.reason === null ? "." : `: ${row.reason}`}`,
          details: { approval: approvalView(row) },
        },
      };
    case "pending":
    case "superseded":
      return { ok: true, approval: "required" };
  }
});

/** The Environment review a human was asked to approve as `approvalId`, whatever the Organization asks now. */
export const reviewedDiff = Effect.fn("Approvals.reviewedDiff")(function* (organizationId: string, approvalId: string) {
  const row = yield* readRow(organizationId, approvalId).pipe(Effect.orDie);
  return row !== undefined && "diff" in row.review ? row.review.diff : null;
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
  const { effects, diff: review } = refused.details as Extract<ApprovalReview, { diff: unknown }>;
  const organizationId = caller.organization.id;
  const recorded = yield* recordAndSweep(organizationId, diff.environment.id, Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.insert(operationApprovals).values({
      organizationId,
      subject: diff.environment.id,
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
  }), staleEnvironment(organizationId, diff.environment));
  return { ...refused, details: { effects, approval: digest, diff: review, approval_id: recorded.id } };
});

/** An operation that may wait for a human: its sweep key, verb, name, preview, and what it destroys. */
export type OperationAsked = {
  /** `server:<machine id>` or `namespace:<name>`. */
  subject: string;
  verb: OperationVerb;
  name: string;
  preview: JsonValue;
  effects: DestructiveEffect[];
};

const requestOperationApproval = Effect.fn("Approvals.requestOperation")(function* (
  caller: Caller,
  asked: OperationAsked,
  digest: string,
) {
  const organizationId = caller.organization.id;
  const operation = { verb: asked.verb, name: asked.name };
  const recorded = yield* recordAndSweep(organizationId, asked.subject, Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.insert(operationApprovals).values({
      organizationId,
      subject: asked.subject,
      requestedByUserId: caller.userId,
      credentialKind: caller.credential.kind,
      credentialId: caller.credential.id,
      command: asked.verb,
      review: { effects: asked.effects, operation: { ...operation, preview: asked.preview } },
      digest,
    }).onConflictDoUpdate({
      target: [operationApprovals.organizationId, operationApprovals.digest],
      targetWhere: sql`${operationApprovals.status} = 'pending'`,
      set: { updatedAt: new Date() },
    }).returning({ id: operationApprovals.id });
    return row ?? (yield* Effect.die("an upsert returned no row"));
  }), staleOperation(asked.verb, digest));
  return {
    code: "approval_required",
    message: `A human must approve this ${asked.verb} of ${asked.name} before it runs.`,
    details: { effects: asked.effects, approval: digest, approval_id: recorded.id, operation },
  } satisfies StoreRefusal;
});

type Gated = { ok: true; approvalId: string | null } | { ok: false; refusal: StoreRefusal };

export const gateOperation = Effect.fn("Approvals.gateOperation")(function* (
  caller: Caller,
  approvalId: string | null,
  asked: OperationAsked,
): Effect.fn.Return<Gated, never, Database> {
  const trusted = yield* trustedApproval(caller.organization.id, approvalId);
  if (!trusted.ok) return trusted;
  if (asked.effects.length === 0 || trusted.approval === "not_required") return { ok: true, approvalId: null };
  const digest = operationDigest(asked);
  if (trusted.approval !== "required" && trusted.approval.approved === digest) return { ok: true, approvalId };
  return { ok: false, refusal: yield* requestOperationApproval(caller, asked, digest).pipe(Effect.orDie) };
});

/** One approval in the caller's Organization, superseded first if its Environment moved on or its preview changed. */
export const getApproval = <R = never>(organizationId: string, id: string, operationDigest?: OperationDigest<R>) =>
  Effect.gen(function* () {
    const row = yield* readRow(organizationId, id);
    if (row === undefined) return yield* new NotFound({ message: "No such approval." });
    return approvalView(yield* freshen(row, operationDigest));
  }).pipe(Effect.withSpan("Approvals.get"));

/** Every approval still waiting on a human in the Organization, newest first. Each is freshened before it counts. */
export const pendingApprovals = <R = never>(organizationId: string, operationDigest?: OperationDigest<R>) =>
  Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const rows = yield* drizzle.select().from(operationApprovals)
      .where(and(eq(operationApprovals.organizationId, organizationId), eq(operationApprovals.status, "pending")))
      .orderBy(desc(operationApprovals.createdAt), desc(operationApprovals.id));
    const fresh = yield* Effect.forEach(rows, (row) => freshen(row, operationDigest));
    return fresh.filter((row) => row.status === "pending").map(approvalView);
  }).pipe(Effect.withSpan("Approvals.pending"));

type Decided = { ok: true; approval: ApprovalView } | { ok: false; refusal: StoreRefusal };

/**
 * Approve exactly the digest the human saw, or deny with a reason. Repeating a decision answers the row; anything
 * else on a row no longer pending, or a digest that isn't the row's, refuses `conflict` with the row.
 */
export const decideApproval = <R = never>(
  caller: Caller,
  id: string,
  decision: ApprovalDecision,
  operationDigest?: OperationDigest<R>,
) => Effect.gen(function* () {
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
      : conflict(`This approval was already decided elsewhere (${current.status}).`, current);
  };
  const row = yield* freshen(found, operationDigest);
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
}).pipe(Effect.withSpan("Approvals.decide"));
