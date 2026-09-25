import "@tanstack/react-start/server-only";
import type { BuildGrantId, BuildReceipt, BuildReceipts, MachineId } from "@ployz/sdk";
import { and, desc, eq, inArray, isNull, sql } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import { organizationIdForDeployment } from "#/db/scope-values.server";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { githubImageBuildSchema, type GithubImageBuild, type SkipReason } from "./image-build";
import { ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES } from "./runtime-contract";
import type { DeploymentContext } from "./runtime-repository.contract";
import { environmentDeployment, environmentDeploymentImageBuild as table, type ImageBuildStatus, type ServerChoice } from "./tables";

/**
 * Image Build rows and every transition they make. Each transition is one guarded update and
 * returns what happened, so no caller re-reads a row to learn it:
 *
 *   start ──▶ building ─┬─ claim for GitHub ─▶ check-in with grant ─▶ report …
 *                       ├─ skip (not once started) ─▶ building, next Builder
 *                       └─ settle ─▶ built | failed | cancelled
 */

const buildReceiptSchema = Schema.Struct({
  fingerprint: Schema.String.check(Schema.isPattern(/^[0-9a-f]{64}$/)),
  machine_id: rustMachineIdSchema,
  image: Schema.Struct({
    reference: Schema.String.check(Schema.isPattern(/^sha256:[0-9a-f]{64}$/)),
    tags: Schema.mutable(Schema.Array(Schema.String)),
    platforms: Schema.mutable(Schema.Array(Schema.String)),
    location: Schema.String,
  }),
});

/**
 * What one Image Build builds: a Git Service of the attempt, named by its image. `buildIndex` is its
 * position among the attempt's builds, so the Engine spreads them across Servers.
 */
export type ImageBuildTarget = { id: string; deploymentId: string; serviceId: string; image: string; buildIndex: number };

export type ImageBuildOutcome =
  | { status: "built"; receipt: BuildReceipt }
  | { status: "failed"; message: string; machineId: MachineId | null }
  | { status: "cancelled" };

export type ImageBuildResult = { imageBuildId: string; image: string; status: ImageBuildStatus };
/** One Builder's go at an Image Build: it settled there, or the Builder didn't take it and the walk moves on. */
export type ImageBuildAttempt = { kind: "settled"; result: ImageBuildResult } | { kind: "skipped"; reason: SkipReason };

/** How long a Builder that isn't last in the walk has to start a build before the next Builder gets it. */
export const START_WITHIN_MINUTES = 3;

type Build = Pick<ImageBuildTarget, "id" | "image">;
/** An Image Build that settled as `status`, as the walk reports it. */
export const settled = (build: Build, status: ImageBuildStatus) =>
  ({ kind: "settled", result: { imageBuildId: build.id, image: build.image, status } }) satisfies ImageBuildAttempt;

/** The row's status once a guarded transition found it moved on. A vanished row reads failed. */
const statusNow = Effect.fn("Deployments.imageBuildStatus")(function* (imageBuildId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ status: table.status }).from(table).where(eq(table.id, imageBuildId)).limit(1);
  return row?.status ?? "failed";
});

/** The attempt's Git Services each get one Image Build. */
export const imageBuildServices = (context: Pick<DeploymentContext, "snapshots">) =>
  context.snapshots.filter((snapshot) => snapshot.config.source.type === "git");

/** Admission fan-out: one building row per Git Service, owned by the attempt's run. Idempotent. */
export const startImageBuilds = Effect.fn("Deployments.startImageBuilds")(function* (context: DeploymentContext, runId: string) {
  const { drizzle } = yield* Database;
  const deploymentId = context.deployment.id;
  const services = imageBuildServices(context);
  const [owned] = yield* drizzle.select({ id: environmentDeployment.id }).from(environmentDeployment).where(and(
    eq(environmentDeployment.id, deploymentId), eq(environmentDeployment.inngestRunId, runId),
    inArray(environmentDeployment.status, [...ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES]),
  )).limit(1);
  if (!owned || !services.length) return [];
  yield* drizzle.insert(table).values(services.map((snapshot) => ({
    organizationId: organizationIdForDeployment(deploymentId), deploymentId, inngestRunId: runId,
    serviceId: snapshot.serviceId, image: snapshot.config.privateDns,
  }))).onConflictDoNothing();
  const rows = yield* drizzle.select({ id: table.id, deploymentId: table.deploymentId, serviceId: table.serviceId, image: table.image })
    .from(table).where(eq(table.deploymentId, deploymentId)).orderBy(table.serviceId);
  return rows.map((row, buildIndex): ImageBuildTarget => ({ ...row, buildIndex }));
});

type StoredImageBuild = typeof table.$inferSelect;
/** An Image Build as stored, narrowed on its Builder: GitHub's has its run and decoded state. */
export type ImageBuildRow =
  | (StoredImageBuild & { builder: "server"; github: null })
  | (StoredImageBuild & { builder: "github"; github: GithubImageBuild; githubRunId: number });

/** Decodes a stored row's GitHub state. Cloud wrote it, so one that doesn't decode is a defect. */
const decodeImageBuild = (row: StoredImageBuild) => Effect.gen(function* () {
  if (row.builder === "server" || row.github === null || row.githubRunId === null) {
    return { ...row, builder: "server", github: null } satisfies ImageBuildRow;
  }
  const github = yield* Schema.decodeUnknownEffect(githubImageBuildSchema)(row.github).pipe(Effect.orDie);
  return { ...row, builder: "github", github, githubRunId: row.githubRunId } satisfies ImageBuildRow;
}) satisfies Effect.Effect<ImageBuildRow>;

/** One Image Build as stored; undefined once gone. */
export const loadImageBuild = Effect.fn("Deployments.loadImageBuild")(function* (imageBuildId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select().from(table).where(eq(table.id, imageBuildId)).limit(1);
  return row && (yield* decodeImageBuild(row));
});

/** An attempt's Image Builds GitHub holds now, loaded in one read. */
export const loadGithubImageBuilds = Effect.fn("Deployments.loadGithubImageBuilds")(function* (inngestRunId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(table).where(and(eq(table.inngestRunId, inngestRunId), eq(table.builder, "github")));
  const decoded = yield* Effect.forEach(rows, decodeImageBuild);
  return decoded.filter((row) => row.builder === "github");
});

/** Records the Server the Engine chose while the row still builds, so the choice shows during the build. */
export const recordServerChoice = Effect.fn("Deployments.recordServerChoice")(function* (imageBuildId: string, machineId: MachineId, serverChoice: ServerChoice) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(table).set({ machineId, serverChoice, updatedAt: new Date() })
    .where(and(eq(table.id, imageBuildId), eq(table.status, "building")));
});

/** GitHub takes a building row: its dispatched run. Settled meanwhile (cancelled): nothing is claimed. */
export const claimForGithub = Effect.fn("Deployments.claimImageBuildForGithub")(function* (build: Build, runId: number, github: GithubImageBuild) {
  const { drizzle } = yield* Database;
  const [claimed] = yield* drizzle.update(table).set({ builder: "github", githubRunId: runId, github, machineId: null, serverChoice: null, updatedAt: new Date() })
    .where(and(eq(table.id, build.id), eq(table.status, "building"))).returning({ id: table.id });
  return claimed ? { kind: "claimed" as const } : settled(build, yield* statusNow(build.id));
});

/**
 * Adds the current Builder to the skip trail and clears what it left, so the next one starts clean.
 * Refused once the build started (GitHub checked in: `started`) or settled: a started build never moves.
 */
export const skipImageBuilder = Effect.fn("Deployments.skipImageBuilder")(function* (build: Build, reason: SkipReason) {
  const { drizzle } = yield* Database;
  const [skipped] = yield* drizzle.update(table).set({
    skips: sql`${table.skips} || ${JSON.stringify([reason])}::jsonb`,
    builder: "server", githubRunId: null, github: null, machineId: null, serverChoice: null, updatedAt: new Date(),
  }).where(and(eq(table.id, build.id), eq(table.status, "building"), isNull(table.checkedInAt))).returning({ id: table.id });
  if (skipped) return { kind: "skipped", reason } satisfies ImageBuildAttempt;
  const status = yield* statusNow(build.id);
  return status === "building" ? { kind: "started" as const } : settled(build, status);
});

/** Skips a Builder that cannot have started the build: nothing checked in for it. */
export const skipUnstarted = (build: Build, reason: SkipReason) => skipImageBuilder(build, reason).pipe(
  Effect.flatMap((skip) => skip.kind === "started"
    ? Effect.die(new Error("An Image Build started without checking in."))
    : Effect.succeed<ImageBuildAttempt>(skip)),
);

/** Whether a GitHub build may still check in; `checkInImageBuild` claims under these conditions and its run's. */
export const awaitsCheckIn = (row: ImageBuildRow) => row.status === "building" && row.checkedInAt === null;

/**
 * The runner of `runId` checks in with the Build Grant just minted for it: the build starts on GitHub.
 * Accepted once, while that run still holds the build. The grant is minted before this claim, so a
 * failed mint claims nothing and the runner may check in again. The "start within" skip clears the
 * run in the same row, so exactly one of it and the check-in wins.
 */
export const checkInImageBuild = Effect.fn("Deployments.checkInImageBuild")(function* (checkIn: {
  imageBuildId: string; runId: number; machineId: MachineId; grant: { id: BuildGrantId; fingerprint: string };
}) {
  const { drizzle } = yield* Database;
  const [checkedIn] = yield* drizzle.update(table).set({
    checkedInAt: sql`now()`, machineId: checkIn.machineId,
    github: sql`jsonb_set(${table.github}, '{grant}', ${JSON.stringify(checkIn.grant)}::jsonb)`, updatedAt: sql`now()`,
  }).where(and(
    eq(table.id, checkIn.imageBuildId), eq(table.status, "building"), eq(table.builder, "github"),
    eq(table.githubRunId, checkIn.runId), isNull(table.checkedInAt),
  )).returning({ id: table.id });
  return checkedIn !== undefined;
});

/**
 * The runner's Build Steps after `received` earlier events, while the build runs and until the
 * runner reported its end. Guarded on `received`, so a report is taken once.
 */
export const recordGithubReport = Effect.fn("Deployments.recordGithubReport")(function* (
  imageBuildId: string, received: number, report: NonNullable<GithubImageBuild["report"]>,
) {
  const { drizzle } = yield* Database;
  const [recorded] = yield* drizzle.update(table).set({
    github: sql`jsonb_set(${table.github}, '{report}', ${JSON.stringify(report)}::jsonb)`, updatedAt: new Date(),
  }).where(and(
    eq(table.id, imageBuildId), eq(table.status, "building"), eq(table.builder, "github"),
    sql`coalesce((${table.github} -> 'report' ->> 'received')::int, 0) = ${received}`,
    sql`coalesce(${table.github} -> 'report' -> 'platforms', 'null'::jsonb) = 'null'::jsonb`,
  )).returning({ id: table.id });
  return recorded !== undefined;
});

/** Settles a building row once and reports the status it has now. Receipts are private evidence and stored encrypted. */
export const settleImageBuild = Effect.fn("Deployments.settleImageBuild")(function* (build: Build, outcome: ImageBuildOutcome) {
  const { drizzle } = yield* Database;
  const encryption = yield* SecretEncryption;
  const now = new Date();
  const patch = outcome.status === "built"
    ? yield* Schema.decodeUnknownEffect(buildReceiptSchema)(outcome.receipt, { onExcessProperty: "error" }).pipe(
        Effect.map((receipt) => ({ status: "built" as const, machineId: receipt.machine_id, encryptedReceipt: encryption.encrypt(JSON.stringify(receipt)) })),
        Effect.orElseSucceed(() => ({ status: "failed" as const, failureMessage: "Completed build evidence is invalid." })),
      )
    : outcome.status === "failed" ? { status: "failed" as const, machineId: outcome.machineId, failureMessage: outcome.message }
    : { status: "cancelled" as const };
  const [updated] = yield* drizzle.update(table).set({ ...patch, finishedAt: now, updatedAt: now })
    .where(and(eq(table.id, build.id), eq(table.status, "building"))).returning({ status: table.status });
  return settled(build, updated?.status ?? (yield* statusNow(build.id)));
});

/** An Image Build is wanted while it builds and its attempt is active and not being cancelled. */
export const imageBuildWanted = Effect.fn("Deployments.imageBuildWanted")(function* (imageBuildId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({
    build: table.status, attempt: environmentDeployment.status, cancelling: environmentDeployment.cancellationRequestedAt,
  }).from(table)
    .innerJoin(environmentDeployment, eq(environmentDeployment.id, table.deploymentId))
    .where(eq(table.id, imageBuildId)).limit(1);
  return row !== undefined && row.build === "building" && row.cancelling === null && ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES.has(row.attempt);
});

/**
 * The latest Build Receipt per image: one attempt's own, or the environment's newest as a reuse hint
 * for the next build. Receipts are optional evidence; an unreadable one just means a fresh build.
 */
export const loadBuildReceipts = Effect.fn("Deployments.loadBuildReceipts")(function* (scope: { deploymentId: string } | { environmentId: string }) {
  const { drizzle } = yield* Database;
  const encryption = yield* SecretEncryption;
  const rows = yield* drizzle.selectDistinctOn([table.image], { image: table.image, receipt: table.encryptedReceipt })
    .from(table).innerJoin(environmentDeployment, eq(environmentDeployment.id, table.deploymentId))
    .where(and(eq(table.status, "built"), "deploymentId" in scope
      ? eq(table.deploymentId, scope.deploymentId)
      : eq(environmentDeployment.environmentId, scope.environmentId)))
    .orderBy(table.image, desc(table.finishedAt));
  const receipts: BuildReceipts = {};
  for (const { image, receipt } of rows) {
    if (!receipt) continue;
    const decoded = yield* Effect.try(() => Schema.decodeUnknownSync(buildReceiptSchema)(JSON.parse(encryption.decrypt(receipt)), { onExcessProperty: "error" }))
      .pipe(Effect.option);
    if (Option.isSome(decoded)) receipts[image] = decoded.value;
  }
  return receipts;
});
