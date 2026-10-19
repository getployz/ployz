import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { Approval, ConfigCommand, DestructiveEffect, DiffView, JsonValue } from "@ployz/sdk";
import { and, desc, eq, inArray, ne, type SQL, sql } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import { canonicalJson } from "#/lib/json";
import { Uuid } from "#/lib/schema";
import {
  type ApprovalDecision,
  type ApprovalReview,
  approvalCommand,
  approvalSubject,
  type OperationVerb,
  type ServiceApply,
} from "#/modules/approvals/approvals";
import { operationApprovals } from "#/modules/approvals/tables";
import { user } from "#/modules/identity/tables";
import { readStore } from "#/modules/config-store/config-store.server";
import { refusedWith } from "#/modules/config-store/store-sdk.server";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import { type Actor, type Caller, callerEnvironments, callerPermissions } from "#/modules/identity/actor";
import { beyondEnvironments, lacks, refusalOf } from "#/modules/organization/access.server";
import type { Permission } from "#/modules/organization/permissions";
import { isOwnedOnlyByAgents } from "#/modules/identity/claim-required.server";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import { uuidFrom } from "#/server/uuid-from";

type ApprovalRow = typeof operationApprovals.$inferSelect;

/** A UUID that only `approvalId` produces, so a second Deploy under one approval replays the first instead of running. */
const approvalUuid = (approvalId: string) => uuidFrom(`ployz.agent.deploy:${approvalId}`);

/**
 * `command` pinned to what one approval allows exactly once: a Deploy keyed by the approval, so a second one replays the
 * first, and a save (the Store's publish) of exactly the reviewed version, which the Store refuses as stale once that save
 * committed.
 */
export const onceFor = (command: ConfigCommand, approvalId: string, reviewed: DiffView | null): ConfigCommand => {
  if (command.command === "admit") return { ...command, id: approvalUuid(approvalId) };
  if (command.command === "publish" && command.version == null && reviewed !== null) return { ...command, version: reviewed.version };
  return command;
};

/** A review as anyone reads it: what runs once approved stays in Cloud, since a Sync's carries the secrets it sets. */
const shownReview = ({ apply: _apply, ...review }: ApprovalReview): ApprovalReview => review;

/** An approval as the CLI reads it. */
function approvalView(row: ApprovalRow) {
  return {
    id: row.id,
    status: row.status,
    digest: row.digest,
    command: row.command,
    reason: row.reason,
    review: shownReview(row.review),
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

/** The names of the people who asked for `approvals`, keyed by their user id, so a card says whom it waits for. */
export const askerNames = Effect.fn("Approvals.askerNames")(function* (approvals: readonly ApprovalView[]) {
  const ids = [...new Set(approvals.flatMap((approval) => approval.requested_by ?? []))];
  if (ids.length === 0) return new Map<string, string>();
  const { drizzle } = yield* Database;
  const people = yield* drizzle.select({ id: user.id, name: user.name }).from(user).where(inArray(user.id, ids));
  return new Map(people.map((person) => [person.id, person.name]));
});

/** Who asked for approval `id`, and what to run as them once approved. */
export const readAsker = Effect.fn("Approvals.readAsker")(function* (organizationId: string, id: string) {
  const row = yield* readRow(organizationId, id);
  return row === undefined ? undefined : { requestedByUserId: row.requestedByUserId, credentialId: row.credentialId, review: row.review };
});

/**
 * Hands a row this decider approved back when what it waited on didn't run: to pending, or superseded when the plan
 * moved, so a later approval reviews what is there now.
 */
export const undoApproval = Effect.fn("Approvals.undo")(function* (organizationId: string, id: string, moved: boolean) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(operationApprovals)
    .set({ status: moved ? "superseded" : "pending", decidedByUserId: null, decidedAt: null, updatedAt: new Date() })
    .where(and(eq(operationApprovals.id, id), eq(operationApprovals.organizationId, organizationId), eq(operationApprovals.status, "approved")));
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
  preview: JsonValue,
) => Effect.Effect<string | null, never, R>;

const freshen = <R>(row: ApprovalRow, operationDigest?: OperationDigest<R>) => Effect.gen(function* () {
  if (row.status !== "pending") return row;
  const review = row.review;
  if ("diff" in review) {
    yield* recordAndSweep(row.organizationId, row.subject, Effect.void, staleEnvironment(row.organizationId, review.diff.environment));
  } else if (operationDigest !== undefined) {
    const fresh = yield* operationDigest(row.organizationId, row.subject, review.operation.verb, row.digest, review.operation.preview);
    yield* recordAndSweep(row.organizationId, row.subject, Effect.void, staleOperation(review.operation.verb, fresh));
  }
  return (yield* readRow(row.organizationId, row.id)) ?? row;
});

/** Protection is part of what was approved, so an approval doesn't survive the Environment being protected or unprotected. */
export function operationDigest({ verb, preview, effects, protected: guarded }: Pick<OperationAsked, "verb" | "preview" | "effects" | "protected">) {
  const reviewed: JsonValue = guarded === undefined ? { preview, effects } : { preview, effects, protected: guarded };
  return `${verb}:${createHash("sha256").update(canonicalJson(reviewed)).digest("hex")}`;
}

/** "the request to deploy production": a denial names what was asked, never the approval's ID. */
const denied = (row: ApprovalRow) => {
  const { verb, name } = approvalSubject(row.command, row.review);
  return `the request to ${verb.toLowerCase()} ${name}`;
};

type Trusted = { ok: true; approval: Exclude<Approval, "not_required"> } | { ok: false; refusal: StoreRefusal };

export const trustedApproval = Effect.fn("Approvals.trusted")(function* (
  organizationId: string,
  approvalId: string | null,
): Effect.fn.Return<Trusted, never, Database> {
  if (approvalId === null) return { ok: true, approval: "required" };
  const row = yield* readRow(organizationId, approvalId).pipe(Effect.orDie);
  if (row === undefined) {
    return { ok: false, refusal: { code: "not_found", message: "No such approval.", details: { approval_id: approvalId } } };
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

const isEffect = Schema.is(Schema.Struct({ kind: Schema.String, node: Schema.String, path: Schema.String }));
const isDiff = Schema.is(Schema.Struct({
  version: Schema.String,
  environment: Schema.Struct({ id: Schema.String, project: Schema.String, name: Schema.String }),
}));

/** The review an `approval_required` refusal carries: the Store's `DestructiveEffect[]` and `DiffView`, by the fields Cloud reads. */
const RefusedReview = Schema.Struct({
  approval: Schema.String,
  effects: Schema.mutable(Schema.Array(Schema.declare<DestructiveEffect>((value): value is DestructiveEffect => isEffect(value)))),
  diff: Schema.declare<DiffView>((value): value is DiffView => isDiff(value)),
  protected: Schema.optional(Schema.Literal(true)),
});

const requester = (caller: Caller) => ({
  requestedByUserId: caller.userId,
  credentialKind: caller.credential.kind,
  credentialId: caller.credential.id,
  via: caller.via,
});

/**
 * A person who asks again for what is pending takes the request over, so their own prompt can approve it: they could
 * run it from `ployz` anyway. Their review comes with it, so a dashboard ask keeps what to run and one from `ployz`
 * runs nothing on approval. An agent asking again takes nothing, so no other agent's credential inherits the request.
 */
const askedAgain = (caller: Caller, review: ApprovalReview) =>
  caller.via === "agent" ? { updatedAt: new Date() } : { updatedAt: new Date(), ...requester(caller), review };

/** A protected ask names its Environment; one from the dashboard also keeps what to run once approved. */
const storeReview = (caller: Caller, command: ConfigCommand, shown: { effects: DestructiveEffect[]; diff: DiffView }, guarded: boolean): ApprovalReview => {
  if (!guarded) return shown;
  const review = { ...shown, protected: { environment: shown.diff.environment.name } };
  return caller.via === "dashboard" ? { ...review, apply: command } : review;
};

/**
 * Record the Store's `approval_required` as one pending approval per digest and answer the refusal with its ID added
 * as `details.approval_id`. Asking again while it is pending answers the same row. Recording sweeps the Environment's
 * approvals for plans the Store moved past, this one too when it arrives late. A refusal marked `protected` asks a
 * protector; a dashboard ask keeps `command` to run as the asker once approved.
 */
export const requestApproval = Effect.fn("Approvals.request")(function* (
  caller: Caller,
  command: ConfigCommand,
  refused: StoreRefusal,
) {
  if (command.command === "remove" && command.environment == null) return yield* requestProjectRemoval(caller, command.project, refused);
  const decoded = Schema.decodeUnknownOption(RefusedReview)(refused.details);
  if (Option.isNone(decoded)) {
    yield* Effect.logWarning("An approval_required refusal named no review; nothing was recorded.");
    return refused;
  }
  const { approval: digest, effects, diff } = decoded.value;
  const guarded = decoded.value.protected === true;
  const organizationId = caller.organization.id;
  const review = storeReview(caller, command, { effects, diff }, guarded);
  const recorded = yield* recordAndSweep(organizationId, diff.environment.id, Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.insert(operationApprovals).values({
      organizationId,
      subject: diff.environment.id,
      ...requester(caller),
      command: approvalCommand(command),
      review,
      digest,
    }).onConflictDoUpdate({
      target: [operationApprovals.organizationId, operationApprovals.digest],
      targetWhere: sql`${operationApprovals.status} = 'pending'`,
      set: askedAgain(caller, review),
    }).returning({ id: operationApprovals.id });
    return row ?? (yield* Effect.die("an upsert returned no row"));
  }), staleEnvironment(organizationId, diff.environment));
  const details = { effects, approval: digest, diff, approval_id: recorded.id };
  return { ...refused, details: guarded ? { ...details, protected: true } : details };
});

/** An operation that may wait for a human: its sweep key, verb, name, preview, and what it destroys. */
export type OperationAsked = {
  /**
   * `server:<machine id>`, `servers:<organization id>`, `organization:<organization id>`, `namespace:<name>`,
   * `volume:<volume id>`, `member:<user id>`, `invite:<invite id>`, `service:<environment id>/<name>`,
   * `environment:<environment id>` or `project:<name>`.
   */
  subject: string;
  verb: OperationVerb;
  name: string;
  preview: JsonValue;
  effects: DestructiveEffect[];
  /** The Protected Environment it lands in when its asker can't protect it, so a protector approves it, even from the dashboard. */
  protected?: string;
  /** What to run as the asker once approved: a dashboard ask that waits for a protector. */
  apply?: ServiceApply;
};

const requestOperationApproval = Effect.fn("Approvals.requestOperation")(function* (
  caller: Caller,
  asked: OperationAsked,
  digest: string,
  stale: Stale<never> = staleOperation(asked.verb, digest),
) {
  const organizationId = caller.organization.id;
  const operation = { verb: asked.verb, name: asked.name };
  const shown = { effects: asked.effects, operation: { ...operation, preview: asked.preview } };
  const guarded = asked.protected === undefined ? shown : { ...shown, protected: { environment: asked.protected } };
  const review: ApprovalReview = asked.apply === undefined ? guarded : { ...guarded, apply: asked.apply };
  const recorded = yield* recordAndSweep(organizationId, asked.subject, Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.insert(operationApprovals).values({
      organizationId,
      subject: asked.subject,
      ...requester(caller),
      command: asked.verb,
      review,
      digest,
    }).onConflictDoUpdate({
      target: [operationApprovals.organizationId, operationApprovals.digest],
      targetWhere: sql`${operationApprovals.status} = 'pending'`,
      set: askedAgain(caller, review),
    }).returning({ id: operationApprovals.id });
    return row ?? (yield* Effect.die("an upsert returned no row"));
  }), stale);
  return {
    code: "approval_required",
    message: `A human must approve this ${asked.verb.replaceAll("_", " ")} of ${asked.name} before it runs.`,
    details: { effects: asked.effects, approval: digest, approval_id: recorded.id, operation, organization: caller.organization.slug },
  } satisfies StoreRefusal;
});

/** A Project's removal reviews each of its Environments, so it waits as an operation on the Project, keyed by the Store's digest. */
const ProjectRemovalReview = Schema.Struct({
  approval: Schema.String,
  effects: RefusedReview.fields.effects,
  diff: Schema.Record(Schema.String, RefusedReview.fields.diff),
});

const requestProjectRemoval = Effect.fn("Approvals.requestProjectRemoval")(function* (
  caller: Caller,
  project: string,
  refused: StoreRefusal,
) {
  const decoded = Schema.decodeUnknownOption(ProjectRemovalReview)(refused.details);
  if (Option.isNone(decoded)) {
    yield* Effect.logWarning("A Project removal's approval_required named no review; nothing was recorded.");
    return refused;
  }
  const { approval: digest, effects, diff } = decoded.value;
  const asked: OperationAsked = { subject: `project:${project}`, verb: "remove", name: project, preview: diff, effects };
  return yield* requestOperationApproval(caller, asked, digest, Effect.succeed(ne(operationApprovals.digest, digest)));
});

type Gated = { ok: true; approvalId: string | null } | { ok: false; refusal: StoreRefusal };

/** Asks a human before `asked` runs, as `gateOperation` does for one Caller and the approval it carries. */
export type Gate = (asked: OperationAsked) => Effect.Effect<Gated, never, Database>;

export const hasApprover = (organizationId: string) =>
  isOwnedOnlyByAgents(organizationId).pipe(Effect.map((agentsOnly) => !agentsOnly), Effect.orDie);

export const gateOperation = Effect.fn("Approvals.gateOperation")(function* (
  caller: Caller,
  approvalId: string | null,
  asked: OperationAsked,
): Effect.fn.Return<Gated, never, Database> {
  // A person who confirmed it in the dashboard already approved it; only `ployz`, agents, and an ask into a Protected
  // Environment by someone who can't protect it wait on one.
  if (caller.via === "dashboard" && asked.protected === undefined) return { ok: true, approvalId: null };
  const trusted = yield* trustedApproval(caller.organization.id, approvalId);
  if (!trusted.ok) return trusted;
  if (asked.effects.length === 0) return { ok: true, approvalId: null };
  if (asked.protected === undefined && !(yield* hasApprover(caller.organization.id))) return { ok: true, approvalId: null };
  const digest = operationDigest(asked);
  if (trusted.approval !== "required" && trusted.approval.approved === digest) return { ok: true, approvalId };
  return { ok: false, refusal: yield* requestOperationApproval(caller, asked, digest).pipe(Effect.orDie) };
});

/** A new ask for `asked` once the approval that covered it has run its one operation. */
export const askAgain = (caller: Caller, asked: OperationAsked) =>
  requestOperationApproval(caller, asked, operationDigest(asked)).pipe(Effect.orDie);

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

/** `decided` is set when this call decided it, not when it answers a decision already made. */
type Decided = { ok: true; approval: ApprovalView; decided?: true } | { ok: false; refusal: StoreRefusal };

/**
 * Who decides: a person in the dashboard with what they hold there, or a Caller through the credential it signed in
 * with. A dashboard decider that names no permissions holds none.
 */
export type Decider = Actor & { readonly organization: { readonly id: string } } & (
  | { readonly via: "dashboard"; readonly permissions?: ReadonlyArray<Permission>; readonly environments?: ReadonlyArray<string> | undefined }
  | Pick<Caller, "via" | "credential" | "role" | "support">
);

const held = (decider: Decider) => "role" in decider && decider.credential !== undefined
  ? { permissions: callerPermissions(decider), environments: callerEnvironments({ credential: decider.credential }) }
  : { permissions: "permissions" in decider ? decider.permissions ?? [] : [], environments: "environments" in decider ? decider.environments : undefined };

/**
 * Why `decider` may not decide `row`, or null when they may. An ask into a Protected Environment is decided by someone
 * who can protect it, other than its asker, however they decide. Any other is denied by
 * any member and approved by a person in Ployz Cloud, or the prompt that asked.
 */
const mayNotDecide = (decider: Decider, row: ApprovalRow, approve: boolean): string | null => {
  const guarded = row.review.protected;
  if (guarded !== undefined) {
    if (decider.userId === row.requestedByUserId) return "You asked for this, so someone else who can protect it decides.";
    const { permissions, environments } = held(decider);
    if (!permissions.includes("environments:protect")) return refusalOf("environments:protect");
    return beyondEnvironments(environments, { environment: guarded.environment });
  }
  if (!approve || decider.via === "dashboard" || row.via === "cli") return null;
  if (row.via === "agent" && row.credentialKind === "token" && decider.userId === row.requestedByUserId) return null;
  return "Only a person in Ployz Cloud, or the prompt that asked, can approve this.";
};

/**
 * Approve exactly the digest the human saw, or deny with a reason. Repeating a decision answers the row; anything
 * else on a row no longer pending, or a digest that isn't the row's, refuses `conflict` with the row. Who may decide is
 * `mayNotDecide`'s. The row records who decided.
 */
export const decideApproval = <R = never>(
  caller: Decider,
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
  if (caller.via === "agent") {
    return { ok: false, refusal: { code: "forbidden", message: "Agents can't answer approvals.", details: { approval: approvalView(found) } } } satisfies Decided;
  }
  const row = yield* freshen(found, operationDigest);
  const lacking = caller.via === "dashboard" ? null : lacks(caller, "approvals:answer");
  if (lacking !== null) {
    return { ok: false, refusal: { code: "forbidden", message: lacking, details: { approval: approvalView(row) } } } satisfies Decided;
  }
  const refused = mayNotDecide(caller, row, "approve" in decision);
  if (refused !== null) {
    return { ok: false, refusal: { code: "forbidden", message: refused, details: { approval: approvalView(row) } } } satisfies Decided;
  }
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
  if (updated !== undefined) return { ok: true, approval: approvalView(updated), decided: true } satisfies Decided;
  const current = yield* readRow(caller.organization.id, id);
  return current === undefined ? yield* new NotFound({ message: "No such approval." }) : settled(current);
}).pipe(Effect.withSpan("Approvals.decide"));
