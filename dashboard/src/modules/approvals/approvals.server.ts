import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { Approval, ConfigCommand, DestructiveEffect, DiffView, EnvironmentRef, JsonValue } from "@ployz/sdk";
import { and, desc, eq, inArray, isNull, ne, type SQL, sql } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import { canonicalJson } from "#/lib/json";
import { Uuid } from "#/lib/schema";
import {
  type ApprovalDecision,
  type ApprovalReview,
  type Redeploy,
  approvalCommand,
  approvalSubject,
  type OperationVerb,
  type ServiceApply,
  type VolumeApply,
} from "#/modules/approvals/approvals";
import { operationApprovals } from "#/modules/approvals/tables";
import { user } from "#/modules/identity/tables";
import { readStore } from "#/modules/config-store/config-store.server";
import { refusedWith } from "#/modules/config-store/store-sdk.server";
import { StoreRefused, type StoreRefusal } from "#/modules/config-store/store.contract";
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
const shownReview = ({ apply: _apply, ...review }: ApprovalReview, subject: string): ApprovalReview =>
  "operation" in review ? { ...review, operation: { ...review.operation, subject } } : review;

/** An approval as the CLI reads it. */
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
    .set({ status: moved ? "superseded" : "pending", decidedByUserId: null, decidedAt: null, usedAt: null, updatedAt: new Date() })
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

/**
 * An operation's approval binds its verb and what it acts on, so it covers no other command or target. Protection is
 * part of what was approved, so an approval doesn't survive the Environment being protected or unprotected.
 */
export function operationDigest({ subject, verb, preview, effects, protected: guarded }: Pick<OperationAsked, "subject" | "verb" | "preview" | "effects" | "protected">) {
  const reviewed: JsonValue = guarded === undefined ? { subject, preview, effects } : { subject, preview, effects, protected: guarded };
  return `${verb}:${createHash("sha256").update(canonicalJson(reviewed)).digest("hex")}`;
}

/** "the request to deploy production": a refusal names what was asked, never the approval's ID. */
const theRequest = (row: ApprovalRow) => {
  const { verb, name } = approvalSubject(row.command, row.review);
  return `the request to ${verb.toLowerCase()} ${name}`;
};

type Trusted = { ok: true; approval: Exclude<Approval, "not_required"> } | { ok: false; refusal: StoreRefusal };

/**
 * What a request carrying an approval runs: its command, as the approval records it, and what it acts on where Cloud
 * can name it (`writeTarget` for a Store write). The Store's digest binds the command and target either way.
 */
export type Redeeming = { readonly command: string; readonly subject?: string };

const AGAIN = "Run the command again without it to ask for";

/**
 * Why `caller` can't run what `row` approved, or null: an approval runs only for the person who asked, through the
 * session or token they asked from, and only for the command and target they asked about.
 */
const notTheirs = (caller: Pick<Caller, "userId" | "credential">, row: ApprovalRow, asked?: Redeeming): string | null => {
  if (row.requestedByUserId !== caller.userId) return `Someone else asked for this approval, so only they can use it. ${AGAIN} your own.`;
  if (row.credentialKind !== caller.credential.kind || row.credentialId !== caller.credential.id) {
    return `This approval was asked for from another session or token. ${AGAIN} one from here.`;
  }
  if (asked !== undefined && (row.command !== asked.command || (asked.subject !== undefined && row.subject !== asked.subject))) {
    return `This approval covers only ${theRequest(row)}. ${AGAIN} one.`;
  }
  return null;
};

type Carried = { ok: true; row: ApprovalRow } | { ok: false; refusal: StoreRefusal };

/** The approval `approvalId`, when `caller` asked for it from this credential, and for `asked` when named. */
const carried = Effect.fn("Approvals.carried")(function* (
  caller: Pick<Caller, "userId" | "organization" | "credential">,
  approvalId: string,
  asked?: Redeeming,
): Effect.fn.Return<Carried, never, Database> {
  const row = yield* readRow(caller.organization.id, approvalId).pipe(Effect.orDie);
  if (row === undefined) {
    return { ok: false, refusal: { code: "not_found", message: "No such approval.", details: { approval_id: approvalId } } };
  }
  const refused = notTheirs(caller, row, asked);
  return refused === null ? { ok: true, row } : { ok: false, refusal: { code: "forbidden", message: refused, details: { approval_id: approvalId } } };
});

/**
 * Why `caller` can't carry approval `approvalId` at all, or null: what a keyed request checks before it replays the
 * operation its key started, since the replay never reaches the gate.
 */
export const unheldApproval = (caller: Pick<Caller, "userId" | "organization" | "credential">, approvalId: string | null) =>
  approvalId === null ? Effect.succeed(null) : Effect.map(carried(caller, approvalId), (held) => held.ok ? null : held.refusal);

/**
 * The approval `approvalId` as `caller` may run it for `asked`: every request that carries an approval redeems it here.
 * One whose operation already ran asks again.
 */
export const redeemApproval = Effect.fn("Approvals.redeem")(function* (
  caller: Pick<Caller, "userId" | "organization" | "credential">,
  approvalId: string | null,
  asked: Redeeming,
): Effect.fn.Return<Trusted, never, Database> {
  if (approvalId === null) return { ok: true, approval: "required" };
  const held = yield* carried(caller, approvalId, asked);
  if (!held.ok) return held;
  const { row } = held;
  switch (row.status) {
    case "approved":
      return row.usedAt === null ? { ok: true, approval: { approved: row.digest } } : { ok: true, approval: "required" };
    case "denied":
      return {
        ok: false,
        refusal: {
          code: "approval_denied",
          message: `A human denied ${theRequest(row)}${row.reason === null ? "." : `: ${row.reason}`}`,
          details: { approval: approvalView(row) },
        },
      };
    case "pending":
    case "superseded":
      return { ok: true, approval: "required" };
  }
});

/**
 * What a Store write acts on, as an approval of it records it: its Environment's ID, or `project:<name>` for a
 * Project's removal. Undefined where the Store can't name it, which then refuses the write itself, and for a Take into
 * its only Destination, whose digest binds that Environment.
 */
export const writeTarget = Effect.fn("Approvals.writeTarget")(function* (organizationId: string, command: ConfigCommand) {
  const listed = (project: string | null) =>
    Effect.map(readStore(organizationId, { query: "environments", project }), ({ environments }) => environments);
  const named = (ref: EnvironmentRef) =>
    Effect.map(listed(ref.project), (environments) => environments.find((listing) => ref.environment == null ? listing.default : listing.name === ref.environment)?.id);
  const target = Effect.gen(function* () {
    switch (command.command) {
      case "remove":
        return command.environment == null ? `project:${command.project}` : yield* named({ project: command.project, environment: command.environment });
      case "publish":
      case "set_environment_protected":
        return yield* named(command.environment);
      case "admit":
        if (command.admit === "retry") return (yield* readStore(organizationId, { query: "deployment", id: command.deployment })).environment.id;
        return "environment" in command ? yield* named(command.environment) : undefined;
      case "sync": {
        if (command.into != null) return yield* named(command.into);
        // Omitted, it lands in the sender's Parent.
        const environments = yield* listed(command.from.project);
        const parent = environments.find((listing) => command.from.environment == null ? listing.default : listing.name === command.from.environment)?.parent;
        return parent == null ? undefined : environments.find((listing) => listing.name === parent)?.id;
      }
      case "take":
        return command.into == null ? undefined : yield* named(command.into);
      default:
        return undefined;
    }
  });
  return yield* target.pipe(Effect.catchIf((error) => error instanceof StoreRefused, () => Effect.succeed(undefined)));
});

/**
 * What a Store write that carries approval `approvalId` redeems it for: its command and, read only then, its target.
 * Protection is an operation on the Environment, so an unprotect's approval is the gate's `unprotect`.
 */
export const redeeming = Effect.fn("Approvals.redeeming")(function* (organizationId: string, command: ConfigCommand, approvalId: string | null) {
  if (command.command === "set_environment_protected") {
    const asked: Redeeming = { command: command.protected ? "protect" : "unprotect" };
    const environment = approvalId === null ? undefined : yield* writeTarget(organizationId, command);
    return environment === undefined ? asked : { ...asked, subject: `environment:${environment}` };
  }
  const asked: Redeeming = { command: approvalCommand(command) };
  if (approvalId === null) return asked;
  const subject = yield* writeTarget(organizationId, command);
  return subject === undefined ? asked : { ...asked, subject };
});

/**
 * The refusal for approval `approvalId` carried into a request that may ask no one, which `asked` names; null when it
 * carries none, or one `caller` may run for exactly this. Checked before anything that can skip the gate, such as a
 * write that destroys nothing or is already done, so a carried approval is refused the same way wherever it lands.
 */
export const carriedInto = (caller: Pick<Caller, "userId" | "organization" | "credential">, approvalId: string | null, asked: Redeeming) =>
  approvalId === null ? Effect.succeed(null) : Effect.map(redeemApproval(caller, approvalId, asked), (redeemed) => redeemed.ok ? null : redeemed.refusal);

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
  redeploy: Schema.optional(Schema.Struct({ deployment: Schema.Number, rolls_back: Schema.mutable(Schema.Array(Schema.Number)) })),
  prunes: Schema.optional(Schema.Literal(true)),
});

const requester = (caller: Caller) => ({
  requestedByUserId: caller.userId,
  credentialKind: caller.credential.kind,
  credentialId: caller.credential.id,
  via: caller.via,
});

/**
 * An approval is one credential's ask for one command, so asking again through the same credential answers the same
 * row, and anyone else's ask is a row of its own. A person asking again brings their review, so a dashboard ask keeps
 * what to run and one from `ployz` runs nothing on approval; an agent asking again through a person's credential takes
 * nothing.
 */
const pending = {
  target: [operationApprovals.organizationId, operationApprovals.digest, operationApprovals.command, operationApprovals.credentialKind, operationApprovals.credentialId],
  targetWhere: sql`${operationApprovals.status} = 'pending'`,
};

const askedAgain = (caller: Caller, review: ApprovalReview) =>
  caller.via === "agent" ? { updatedAt: new Date() } : { updatedAt: new Date(), via: caller.via, review };

/** A protected ask names its Environment; one from the dashboard also keeps what to run once approved. */
const storeReview = (
  caller: Caller,
  command: ConfigCommand,
  shown: { effects: DestructiveEffect[]; diff: DiffView; redeploy?: Redeploy; prunes?: true },
  guarded: boolean,
): ApprovalReview => {
  if (!guarded) return shown;
  const review = { ...shown, protected: { environment: shown.diff.environment.name } };
  return caller.via === "dashboard" ? { ...review, apply: command } : review;
};

/**
 * The refusal for a write that carried approval `carried` into another target than the one it approves, which the
 * Store names as it asks again; null when it carried none, or one for this target whose plan moved.
 */
const carriedElsewhere = (caller: Caller, carried: string | null, command: ConfigCommand, subject: string) =>
  redeemApproval(caller, carried, { command: approvalCommand(command), subject }).pipe(
    Effect.map((redeemed) => redeemed.ok ? null : redeemed.refusal),
  );

/**
 * Record the Store's `approval_required` as one pending approval per digest and answer the refusal with its ID added
 * as `details.approval_id`. Asking again while it is pending answers the same row. Recording sweeps the Environment's
 * approvals for plans the Store moved past, this one too when it arrives late. A refusal marked `protected` asks a
 * protector; a dashboard ask keeps `command` to run as the asker once approved. A write that carried an approval for
 * another target is refused instead.
 */
export const requestApproval = Effect.fn("Approvals.request")(function* (
  caller: Caller,
  command: ConfigCommand,
  refused: StoreRefusal,
  carried: string | null,
) {
  if (command.command === "remove" && command.environment == null) {
    return (yield* carriedElsewhere(caller, carried, command, `project:${command.project}`)) ?? (yield* requestProjectRemoval(caller, command.project, refused));
  }
  const decoded = Schema.decodeUnknownOption(RefusedReview)(refused.details);
  if (Option.isNone(decoded)) {
    yield* Effect.logWarning("An approval_required refusal named no review; nothing was recorded.");
    return refused;
  }
  const { approval: digest, effects, diff, redeploy, prunes } = decoded.value;
  const elsewhere = yield* carriedElsewhere(caller, carried, command, diff.environment.id);
  if (elsewhere !== null) return elsewhere;
  const guarded = decoded.value.protected === true;
  const organizationId = caller.organization.id;
  // A redeploy's card says what it rolls back, and a pruning Deploy's that it prunes, which its digest binds.
  const shown = { effects, diff, ...(redeploy && { redeploy }), ...(prunes && { prunes }) };
  const review = storeReview(caller, command, shown, guarded);
  const recorded = yield* recordAndSweep(organizationId, diff.environment.id, Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.insert(operationApprovals).values({
      organizationId,
      subject: diff.environment.id,
      ...requester(caller),
      command: approvalCommand(command),
      review,
      digest,
    }).onConflictDoUpdate({ ...pending, set: askedAgain(caller, review) }).returning({ id: operationApprovals.id });
    return row ?? (yield* Effect.die("an upsert returned no row"));
  }), staleEnvironment(organizationId, diff.environment));
  const asked = { effects, approval: digest, diff, approval_id: recorded.id, ...(prunes && { prunes }) };
  const details = redeploy ? { ...asked, redeploy: { deployment: redeploy.deployment, rolls_back: redeploy.rolls_back } } : asked;
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
  apply?: ServiceApply | VolumeApply;
  /**
   * Its run records the approval it ran under, once, so carrying that approval again answers or finishes that run
   * instead of starting another. The gate leaves such an approval unspent; it spends every other one on its one run.
   */
  recorded?: true;
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
    }).onConflictDoUpdate({ ...pending, set: askedAgain(caller, review) }).returning({ id: operationApprovals.id });
    return row ?? (yield* Effect.die("an upsert returned no row"));
  }), stale);
  // The subject rides in the refusal, not the stored review: approvalView adds it from the row.
  const details = {
    effects: asked.effects,
    approval: digest,
    approval_id: recorded.id,
    operation: { ...operation, subject: asked.subject },
    organization: caller.organization.slug,
  };
  return {
    code: "approval_required",
    message: `A human must approve this ${asked.verb.replaceAll("_", " ")} of ${asked.name} before it runs.`,
    details: asked.protected === undefined ? details : { ...details, protected: { environment: asked.protected } },
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
  // Environment by someone who can't protect it wait on one. That ask waits even when it destroys nothing.
  if (caller.via === "dashboard" && asked.protected === undefined) return { ok: true, approvalId: null };
  const trusted = yield* redeemApproval(caller, approvalId, { command: asked.verb, subject: asked.subject });
  if (!trusted.ok) return trusted;
  if (asked.effects.length === 0 && asked.protected === undefined) return { ok: true, approvalId: null };
  if (asked.protected === undefined && !(yield* hasApprover(caller.organization.id))) return { ok: true, approvalId: null };
  const digest = operationDigest(asked);
  if (approvalId !== null && trusted.approval !== "required" && trusted.approval.approved === digest) {
    if (asked.recorded || (yield* spend(caller.organization.id, approvalId))) return { ok: true, approvalId };
  }
  return { ok: false, refusal: yield* requestOperationApproval(caller, asked, digest).pipe(Effect.orDie) };
});

/**
 * Spend approval `approvalId` on the one operation it lets through: true for the one run that does, false for any
 * other, which asks again. One statement, so two runs racing on one approval can't both spend it.
 */
const spend = Effect.fn("Approvals.spend")(function* (organizationId: string, approvalId: string) {
  const { drizzle } = yield* Database;
  const spent = yield* drizzle.update(operationApprovals).set({ usedAt: new Date() }).where(and(
    eq(operationApprovals.id, approvalId),
    eq(operationApprovals.organizationId, organizationId),
    eq(operationApprovals.status, "approved"),
    isNull(operationApprovals.usedAt),
  )).returning({ id: operationApprovals.id }).pipe(Effect.orDie);
  return spent.length > 0;
});

/** Hand back an approval the gate spent on a run that then refused, so repeating that run needs no new approval. */
export const unspendApproval = Effect.fn("Approvals.unspend")(function* (organizationId: string, approvalId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(operationApprovals).set({ usedAt: null })
    .where(and(eq(operationApprovals.id, approvalId), eq(operationApprovals.organizationId, organizationId), eq(operationApprovals.status, "approved")))
    .pipe(Effect.orDie);
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
