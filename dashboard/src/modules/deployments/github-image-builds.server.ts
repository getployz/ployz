import "@tanstack/react-start/server-only";
import type { BuildGrantId, BuildReceipt, MachineId, PreparationEvent } from "@ployz/sdk";
import { Effect, Schema, type Types } from "effect";
import { cancelGithubRun, checkGithubBuildWorkflow, dispatchGithubBuildWorkflow, githubRunCompleted } from "#/modules/github/github-build.server";
import { verifyGithubOidcToken } from "#/modules/github/github-oidc.server";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createGithubBuildRunCompletedEvent } from "#/modules/inngest/events";
import { buildFingerprints, buildGrantTag, ployzVersion } from "#/modules/runtime/ployz.server";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { BuildGrantUnavailable, Conflict, Forbidden, NotFound, Unauthorized, Validation } from "#/server/public-error";
import type { BuildCandidate } from "./build-order";
import { githubSkipReason, installFailedSchema, type GithubImageBuild } from "./image-build";
import { openBuildStep, persistBuildLog } from "./deployment-events.server";
import {
  awaitsCheckIn, checkInImageBuild, closeOpenSteps, claimForGithub, loadBuildReceipts, loadGithubImageBuilds, imageBuildNow, loadImageBuild, recordGithubReport, recordServerChoice, settleGithubImageBuild, settleImageBuild,
  moveStartedGithubBuild, skipImageBuilder, skipUnstarted, START_WITHIN_MINUTES,
  type ImageBuildAttempt, type ImageBuildRow, type ImageBuildTarget,
} from "./image-builds.server";
import { ployzStep, preparationProgressCollector, type BuildOutputWrite, type BuildStepWrite } from "./preparation-progress";
import { loadDeploymentContext } from "./runtime-hydration.repository.server";
import type { DeploymentContext } from "./runtime-repository.contract";
import { connectedRuntime, DeploymentRuntimeUnavailable, oneServiceDeployment } from "./runtime-session.server";
import { pinSourceCommit } from "./runtime-sources.server";
import { reusedImageStep } from "./server-image-builds.server";

/**
 * GitHub as a Builder. Cloud dispatches the repository's build workflow, the runner checks in once
 * with its OIDC token for the build's grant and secrets, posts its Build Steps as it builds, and
 * once it pushed, its final report makes Cloud end the grant and write the receipt from the digest
 * the Machine received. The run then goes on uploading build cache, which Cloud never waits for.
 *
 *   dispatch ──▶ check-in (once) ──▶ steps … ──▶ final report ──▶ end grant ──▶ receipt
 *
 * Check-in is the build starting. Until then GitHub can still be skipped: at once when it can't take
 * the build, at the "start within" limit, or when the run ends first. After it, only a failed Build
 * Step fails the build; a run that fails for GitHub's reasons (no final report, no push, out of
 * budget) is skipped too. The run completing (the Workflow run webhook) or the budget running out
 * settles only a build that never reported its end; once settled, a failed or cancelled run changes nothing.
 */

/**
 * How long a run may build after it checked in before Cloud cancels it. The Build Grant is minted at
 * check-in and must outlive this: the daemon's `GRANT_LIFETIME` (3h, ployzd `management/build_grant.rs`).
 */
export const GITHUB_RUN_BUDGET_MS = 2 * 60 * 60_000;
/** How often Cloud looks at a dispatched run on GitHub between Workflow run webhooks. */
export const GITHUB_CHECK_INTERVAL = "10m";

export type GithubBuildStart = ImageBuildAttempt | { kind: "dispatched"; runId: number };
/** What one look at a dispatched GitHub build finds: it settled, GitHub was skipped, or it goes on. */
export type GithubBuildCheck = ImageBuildAttempt | { kind: "waiting" };
const waiting = { kind: "waiting" } satisfies GithubBuildCheck;

type Snapshot = DeploymentContext["snapshots"][number];
const installedSource = (snapshot: Snapshot | undefined) => {
  const source = snapshot?.config.source;
  return source?.type === "git" && source.access.type === "github-installation"
    ? { ...source, installationId: source.access.installationId } : null;
};

/** The one repository a runner's Build Grant may push into. */
const grantRepository = (image: string) => `ployz-build/${image}`;

/**
 * Settles an Image Build built, dispatching nothing, when the Service's latest receipt is for this
 * commit and the Cluster still holds its image. Otherwise dispatches it to GitHub Actions on the
 * native runner for its one platform, and records why GitHub took it. GitHub is skipped at once,
 * with the reason on the Image Build, when it can't take the build: the repository isn't reachable
 * through the GitHub App or lacks permission, has no workflow, needs several platforms, or the
 * dispatch fails.
 */
export const startGithubImageBuild = Effect.fn("Deployments.startGithubImageBuild")(function* (build: ImageBuildTarget, candidate: Pick<BuildCandidate, "reason" | "attempt">) {
  const owner = { image: build.image, attempt: candidate.attempt };
  const context = yield* loadDeploymentContext(build.deploymentId);
  const snapshot = context?.snapshots.find((candidate) => candidate.serviceId === build.serviceId);
  const source = installedSource(snapshot);
  if (!context || !snapshot || !source) return yield* skipUnstarted(build, { builder: "github", kind: "not_connected" });
  const workflow = yield* checkGithubBuildWorkflow(source.installationId, source.repositoryId);
  const repository = workflow.fullName ?? source.repository;
  if (workflow.readiness === "no_permission") return yield* skipUnstarted(build, { builder: "github", kind: "no_permission", repository });
  if (workflow.readiness !== "ready" || !workflow.fullName || !workflow.defaultBranch) {
    return yield* skipUnstarted(build, { builder: "github", kind: "no_workflow", repository });
  }
  const deployment = yield* oneServiceDeployment(context, build.serviceId);
  // The check-in hands the runner this pinned commit.
  const commit = yield* pinSourceCommit(context, snapshot, source);
  const hint = (yield* loadBuildReceipts({ environmentId: context.environment.id }))[build.image];
  const outside = yield* Effect.gen(function* () {
    const sdk = yield* connectedRuntime(context.organization.id);
    // An unchanged commit whose image the Cluster still holds is built already, as on the servers.
    return yield* sdk.outsideBuild({ deployment, commit, receipt: hint }).pipe(Effect.catch((error) => hint
      // Reuse is only a shortcut: a receipt that can't be checked means GitHub builds.
      ? Effect.logWarning("Could not check a receipt for reuse; GitHub builds.", error).pipe(Effect.andThen(sdk.outsideBuild({ deployment, commit })))
      : Effect.fail(error)));
  }).pipe(Effect.scoped);
  if (outside.kind === "reuse") {
    yield* recordServerChoice(build.id, outside.receipt.machine_id, { machineName: outside.machine_name, reason: { kind: "reused" } });
    yield* persistBuildLog(build.deploymentId, { steps: [reusedImageStep()], output: [] }, owner);
    return yield* settleImageBuild(build, { status: "built", receipt: outside.receipt });
  }
  const { platforms } = outside;
  if (platforms.length > 1) {
    return yield* skipUnstarted(build, { builder: "github", kind: "multi_platform", platforms: platforms.map((platform) => platform.replace(/^linux\//, "")) });
  }
  // With no visible placement, deploy's coverage check decides, as it does after a server build.
  const runner = platforms[0] === "linux/arm64" ? "ubuntu-24.04-arm" : "ubuntu-latest";
  const config = yield* AppConfig;
  // Cloud sees both ends of the wait for a runner. It opens before the dispatch, so even the fastest
  // check-in finds it to close, and only once, so a retried start never reopens a closed wait.
  yield* openBuildStep(build.deploymentId, owner, ployzStep("runner", "Waiting for a runner", new Date(), null));
  const run = yield* dispatchGithubBuildWorkflow({
    installationId: source.installationId, fullName: workflow.fullName, defaultBranch: workflow.defaultBranch,
    inputs: { build: build.id, cloud: config.app.url.origin, runner },
  });
  const claim = yield* claimForGithub(build, run.runId, {
    runUrl: run.runUrl, fullName: workflow.fullName, workflowRef: run.workflowRef, reason: candidate.reason, grant: null, report: null,
  });
  if (claim.kind === "settled") {
    // Settled (cancelled) while dispatching: the run must not build.
    yield* cancelGithubRun({ installationId: source.installationId, fullName: workflow.fullName, runId: run.runId }).pipe(Effect.ignore);
    // It settled before the wait opened: nothing else closes it.
    yield* closeOpenSteps(build.id);
    return claim satisfies GithubBuildStart;
  }
  return { kind: "dispatched", runId: run.runId } satisfies GithubBuildStart;
}, (effect, build) => effect.pipe(
  // A GitHub or SDK failure before dispatch is GitHub being unusable, not the build failing.
  Effect.catch((error) => skipUnstarted(build, { builder: "github", kind: "dispatch_failed", message: error.message })),
));

type GithubRow = Extract<ImageBuildRow, { builder: "github" }>;

/**
 * One look at a dispatched build, after the Workflow run webhook reported its run `ended` or a wait
 * timed out. A timeout also asks GitHub whether the run completed, which catches a completion that
 * landed between two waits. `startLimit`: this Builder isn't last and its "start within" passed.
 * The last Builder waits for its run to start without a limit; once started, a run gets
 * GITHUB_RUN_BUDGET_MS.
 */
export const checkGithubImageBuild = Effect.fn("Deployments.checkGithubImageBuild")(function* (
  build: ImageBuildTarget, seen: { ended: boolean; startLimit: boolean },
) {
  const row = yield* loadImageBuild(build.id);
  if (row?.status !== "building" || row.builder !== "github") return yield* imageBuildNow(build);
  if (overBudget(row)) return yield* finishGithubImageBuild(build, row, true);
  if (seen.ended || row.github.report?.platforms || (yield* githubRunEnded(row))) return yield* finishGithubImageBuild(build, row, false);
  if (row.checkedInAt === null) return seen.startLimit ? yield* withdrawGithubImageBuild(build, row) : waiting;
  return waiting;
});

const overBudget = (row: GithubRow) => row.checkedInAt !== null && Date.now() - row.checkedInAt.getTime() > GITHUB_RUN_BUDGET_MS;

/**
 * The build's result once it settled, which a final report may do before the walk begins a wait;
 * null while it still builds. A final report that left the build unsettled is acted on here: it
 * ends the grant and settles the build, or moves it on when GitHub failed it, so the walk doesn't
 * wait for a wake it may have missed.
 */
export const settleOrMoveReportedGithubBuild = Effect.fn("Deployments.settleOrMoveReportedGithubBuild")(function* (build: ImageBuildTarget) {
  const row = yield* loadImageBuild(build.id);
  if (row?.status !== "building" || row.builder !== "github") return yield* imageBuildNow(build);
  if (!row.github.report?.platforms) return null;
  const found = yield* finishGithubImageBuild(build, row, false);
  return found.kind === "waiting" ? null : found;
});

/** Whether GitHub says the build's run completed; unknown (GitHub unreachable) reads as still running. */
const githubRunEnded = (row: GithubRow) => githubRun(row).pipe(
  Effect.flatMap((run) => run ? githubRunCompleted(run) : Effect.succeed(false)),
  Effect.orElseSucceed(() => false),
);

/** Where a build's run lives on GitHub, through the Service's GitHub App installation. */
const githubRun = (row: GithubRow) => loadDeploymentContext(row.deploymentId).pipe(Effect.map((context) => {
  const source = installedSource(context?.snapshots.find((snapshot) => snapshot.serviceId === row.serviceId));
  return source ? { installationId: source.installationId, fullName: row.github.fullName, runId: row.githubRunId } : null;
}));

/**
 * GitHub's "start within" limit passed. A run that checked in has started and keeps the build;
 * otherwise GitHub is skipped and its run cancelled. Check-in and this skip update the same row
 * under exclusive conditions, so exactly one wins.
 */
const withdrawGithubImageBuild = Effect.fn("Deployments.withdrawGithubImageBuild")(function* (build: ImageBuildTarget, row: GithubRow) {
  const skip = yield* skipImageBuilder(build, { builder: "github", kind: "not_started", minutes: START_WITHIN_MINUTES });
  if (skip.kind === "skipped") yield* cancelGithubBuildRun(row);
  return skip.kind === "started" ? waiting : skip;
});

const bearer = (request: Request) => {
  const match = /^Bearer (\S+)$/.exec(request.headers.get("authorization") ?? "");
  return match?.[1] ?? null;
};

/** The runner's OIDC token must name this build's repository, workflow on the default branch, and run. */
const authorizeRunner = Effect.fn("Deployments.authorizeGithubRunner")(function* (request: Request, imageBuildId: string) {
  const token = bearer(request);
  if (!token) return yield* new Unauthorized();
  const config = yield* AppConfig;
  const claims = yield* verifyGithubOidcToken(token, config.app.url.origin).pipe(Effect.mapError(() => new Unauthorized()));
  const row = yield* loadImageBuild(imageBuildId);
  if (row?.builder !== "github") return yield* new NotFound({ message: "No GitHub build has this id." });
  const context = yield* loadDeploymentContext(row.deploymentId);
  const source = installedSource(context?.snapshots.find((snapshot) => snapshot.serviceId === row.serviceId));
  if (!context || !source) return yield* new NotFound({ message: "No GitHub build has this id." });
  if (claims.repository_id !== String(source.repositoryId)) return yield* new Forbidden({ message: "The token is for another repository." });
  if (claims.job_workflow_ref !== row.github.workflowRef) return yield* new Forbidden({ message: "The token is for another workflow or branch." });
  if (claims.run_id !== String(row.githubRunId)) return yield* new Forbidden({ message: "The token is for another run." });
  if (claims.event_name !== "workflow_dispatch") return yield* new Forbidden({ message: "The run was not dispatched by Ployz." });
  return { row, context };
});

/**
 * The runner's one check-in: the build starts. Accepted once, while GitHub still holds the build.
 * Mints a Build Grant on the Machine Cloud deploys through and returns it with the commit, the
 * expected fingerprint, the ployz version that computed it, and the frozen deployment whose `resolvedEnv` carries the build secrets.
 * Nothing secret is ever a workflow input.
 */
export const checkInGithubBuild = Effect.fn("Deployments.checkInGithubBuild")(function* (request: Request, imageBuildId: string) {
  const { row, context } = yield* authorizeRunner(request, imageBuildId);
  const refused = new Conflict({ message: "This build already checked in or is no longer wanted." });
  // Only an early exit, so a refused runner mints nothing; the claim below decides.
  if (!awaitsCheckIn(row)) return yield* refused;
  const snapshot = context.snapshots.find((candidate) => candidate.serviceId === row.serviceId);
  const source = snapshot?.config.source;
  if (!snapshot || source?.type !== "git") return yield* new NotFound({ message: "No GitHub build has this id." });
  const commit = yield* pinSourceCommit(context, snapshot, source);
  const deployment = yield* oneServiceDeployment(context, row.serviceId);
  const fingerprint = buildFingerprints({ deployment, source_commits: { [row.image]: commit } })[row.image];
  if (!fingerprint) return yield* new Validation({ message: "The build has no fingerprint." });
  const { minted, machine } = yield* Effect.gen(function* () {
    const sdk = yield* connectedRuntime(context.organization.id);
    // Inspect first: a failure after the mint would leave an unclaimed grant.
    const machine = yield* sdk.inspect();
    const minted = yield* sdk.mintBuildGrant(grantRepository(row.image)).pipe(
      Effect.tapError((error) => Effect.logWarning("Could not mint a Build Grant.", error)),
      Effect.mapError((cause) => new BuildGrantUnavailable({ cause })),
    );
    return { minted, machine };
  }).pipe(Effect.scoped, Effect.tapError((error) => checkInFailed(row, error)));
  const grant = { id: minted.id, fingerprint };
  const checkedIn = yield* checkInImageBuild({ imageBuildId: row.id, runId: row.githubRunId, machineId: machine.id, grant });
  if (!checkedIn) {
    // Lost to a second check-in or to the start-within skip during a slow mint (rare). The grant's
    // secret never left Cloud, so a grant that fails to end is unusable anyway.
    yield* endGrant(row.organizationId, machine.id, minted.id).pipe(
      Effect.catch((error) => Effect.logWarning("Could not end an unclaimed Build Grant.", error)),
    );
    return yield* refused;
  }
  // The runner arrived: its wait ends.
  yield* closeOpenSteps(row.id);
  // The runner installs this version: the process that computed the fingerprint names it, even mid-rollout.
  return { grant: minted.grant, commit, fingerprint, ployzVersion: ployzVersion(), deployment };
});

/** Why Cloud couldn't start a build its runner checked in for, in the words the log shows. */
const checkInFailure = (error: Error) => error instanceof BuildGrantUnavailable ? "Your Machine couldn't issue a grant to receive the image."
  : error instanceof DeploymentRuntimeUnavailable ? "Cloud couldn't reach your Machine to receive the image."
  : "Cloud couldn't start the build on GitHub.";

/**
 * A check-in the runner arrived for but Cloud couldn't start: the one internal GitHub step the log
 * shows, and only failed. Best effort; the run then ends and the walk moves on or fails. While
 * GitHub holds the build, its go is the skip trail's length.
 */
const checkInFailed = (row: GithubRow, error: Error) => Effect.gen(function* () {
  yield* closeOpenSteps(row.id);
  yield* persistBuildLog(row.deploymentId, { steps: [{ ...ployzStep("check-in", "Starting the build"), error: checkInFailure(error) }], output: [] },
    { image: row.image, attempt: row.skips.length });
}).pipe(Effect.catch((error) => Effect.logWarning("Could not log a failed GitHub check-in.", error)));

const buildStepSchema = Schema.Struct({
  id: Schema.String, name: Schema.String, started: Schema.NullOr(Schema.String), completed: Schema.NullOr(Schema.String),
  cached: Schema.Boolean, error: Schema.NullOr(Schema.String),
});
/** One `ployz build --events` line's event: the Build progress a server build reports too. */
const buildEventSchema = Schema.Struct({ Build: Schema.Union([
  Schema.Struct({ Stage: Schema.String }),
  Schema.Struct({ Output: Schema.mutable(Schema.Array(Schema.Number)) }),
  Schema.Struct({ Step: buildStepSchema }),
  Schema.Struct({ StepOutput: Schema.Struct({ step: Schema.String, stderr: Schema.Boolean, text: Schema.String }) }),
  Schema.Struct({ Timing: Schema.Unknown }),
  Schema.Struct({ Target: Schema.Struct({ name: Schema.String, outcome: Schema.Unknown }) }),
]) }) satisfies Schema.Schema<PreparationEvent>;
/**
 * One batch of the runner's `ployz build --events` lines, starting at line `from` (0-based), and
 * once the build ended, the platforms it built (empty: it failed).
 */
const stepsReportSchema = Schema.Struct({
  from: Schema.Int.check(Schema.isGreaterThanOrEqualTo(0)),
  events: Schema.Array(Schema.Struct({ at: Schema.Number, event: buildEventSchema })),
  platforms: Schema.optionalKey(Schema.Array(Schema.String)),
  /** The ployz version the runner couldn't install; it sends this with empty `platforms`. */
  installFailed: Schema.optionalKey(installFailedSchema),
});

const MAX_STEPS_REPORT_BYTES = 16 * 1024 * 1024;

/**
 * The runner's Build Steps, filed under the Image Build like a server build's, so they show in the
 * same log while it builds. Taken in batches after check-in until the runner reports its end; a
 * batch repeating lines already taken (a retry) files only the new ones.
 */
export const recordGithubBuildSteps = Effect.fn("Deployments.recordGithubBuildSteps")(function* (request: Request, imageBuildId: string, text: string) {
  const { row } = yield* authorizeRunner(request, imageBuildId);
  const github = row.github;
  if (text.length > MAX_STEPS_REPORT_BYTES) return yield* new Validation({ message: "The report is too large." });
  const report = yield* Schema.decodeUnknownEffect(Schema.fromJsonString(stepsReportSchema))(text)
    .pipe(Effect.mapError(() => new Validation({ message: "The report is not Build Steps." })));
  if (row.checkedInAt === null) return yield* new Conflict({ message: "This build has not checked in." });
  if (row.status !== "building" || github.report?.platforms) return yield* new Conflict({ message: "This build already ended." });
  const received = github.report?.received ?? 0;
  if (report.from > received) return yield* new Conflict({ message: `Build Steps before line ${report.from} are missing; send from line ${received}.` });
  let at = new Date();
  const collector = preparationProgressCollector(() => at, github.report?.collector);
  const steps: BuildStepWrite[] = [];
  const output: BuildOutputWrite[] = [];
  for (const line of report.events.slice(received - report.from)) {
    at = new Date(line.at);
    const writes = collector.event(line.event);
    steps.push(...writes.steps);
    output.push(...writes.output);
  }
  const platforms = report.platforms ?? null;
  // A failed Build Step already shows in its row; a build GitHub failed moves on without one.
  if (platforms) steps.push(...collector.finish());
  const taken = Math.max(received, report.from + report.events.length);
  const recorded: Types.Mutable<NonNullable<GithubImageBuild["report"]>> = { received: taken, collector: collector.checkpoint(), platforms: platforms && [...platforms] };
  if (platforms && report.installFailed !== undefined) recorded.installFailed = report.installFailed;
  // The steps land with the report that takes them, in one transaction: a report that lost to a move
  // (or another report) files nothing, so none reopens in GitHub's section after the move closed it.
  // While GitHub holds the build, its go is the skip trail's length.
  const database = yield* Database;
  const accepted = yield* database.transaction(Effect.gen(function* () {
    if (!(yield* recordGithubReport(row.id, received, recorded))) return false;
    yield* persistBuildLog(row.deploymentId, { steps, output }, { image: row.image, attempt: row.skips.length });
    return true;
  }));
  if (!accepted) return yield* new Conflict({ message: "Another report of this build was taken first." });
  if (platforms) {
    // The runner reports its end once the image is pushed, before its run completes (it still
    // uploads build cache), so the report settles the build and wakes the waiting walk. A build
    // GitHub failed is left to the walk to move on. A failure after the report was recorded leaves
    // settling to the run's completion or the poll.
    yield* endGithubBuild(row, { ...row, github: { ...github, report: recorded } }, false);
    yield* sendInngestEvent(createGithubBuildRunCompletedEvent({ id: `reported-${row.id}`, runId: row.githubRunId })).pipe(
      // The run's completion wakes it anyway.
      Effect.catch((error) => Effect.logWarning("Could not wake the walk for a reported GitHub build.", error)),
    );
  }
  return { received: taken };
});

/**
 * The walk's end of a GitHub build, once its runner reported its end, its run ended, or it ran out
 * of budget (`timedOut`). A run that ended before it checked in never started, so GitHub is
 * skipped; a started run GitHub failed moves on to the next Builder.
 */
const finishGithubImageBuild = Effect.fn("Deployments.finishGithubImageBuild")(function* (build: Pick<ImageBuildTarget, "id" | "image">, row: GithubRow, timedOut: boolean) {
  if (row.checkedInAt === null) {
    const skip = yield* skipImageBuilder(build, { builder: "github", kind: "ended_before_start" });
    // It checked in just now; the next check finds its run ended on GitHub and finishes it.
    return skip.kind === "started" ? waiting : skip;
  }
  const ended = yield* endGithubBuild(build, row, timedOut);
  if (ended.kind !== "move") return ended;
  return yield* moveStartedGithubBuild(build, row.githubRunId, ended.reason);
});

/**
 * Ends a started GitHub build's grant (`timedOut`: its run is cancelled first) and settles it:
 * built, with the receipt from the digest the Machine verified, or failed at a Build Step. Without
 * an image and a failed step, GitHub failed it: `move`, with why.
 */
const endGithubBuild = Effect.fn("Deployments.endGithubBuild")(function* (build: Pick<ImageBuildTarget, "id" | "image">, row: GithubRow, timedOut: boolean) {
  if (timedOut) yield* cancelGithubBuildRun(row);
  const failed = (message: string) => settleGithubImageBuild(build, row.githubRunId, { status: "failed", message, machineId: row.machineId });
  const grant = row.github.grant;
  if (!grant || !row.machineId) return yield* failed("GitHub: the run ended before it received its grant.");
  // Only the Machine's answer decides; while it can't be reached the build stays unsettled and the
  // next check (the run's completion or the poll) ends the grant again, until the run's budget is
  // spent: a Machine gone for good must not hold the deploy forever.
  const ended = yield* endGrant(row.organizationId, row.machineId, grant.id).pipe(
    Effect.map((answer) => ({ pushed: answer.pushed ?? null })),
    Effect.catch((error) => Effect.logWarning("Could not end a GitHub build's grant; the next check retries.", error).pipe(Effect.as(null))),
  );
  if (!ended) {
    return timedOut || overBudget(row) ? yield* failed("GitHub: your Machine couldn't be reached to confirm the push within 2 hours.") : waiting;
  }
  const pushed = ended.pushed;
  const platforms = row.github.report?.platforms;
  if (!pushed || !platforms?.length) {
    const reason = githubSkipReason(row.github.report, timedOut);
    return reason ? ({ kind: "move", reason } as const) : yield* failed("GitHub: a build step failed.");
  }
  const receipt: BuildReceipt = {
    fingerprint: grant.fingerprint, machine_id: row.machineId,
    image: { reference: pushed, tags: [buildGrantTag(grantRepository(row.image), pushed)], platforms: [...platforms], location: "build-grant" },
  };
  return yield* settleGithubImageBuild(build, row.githubRunId, { status: "built", receipt });
});

/** Best effort: cancel the run and end its grant, so the grant refuses any further push. */
const cancelGithubBuildRun = (row: GithubRow) => Effect.gen(function* () {
  const run = yield* githubRun(row);
  if (run) yield* cancelGithubRun(run).pipe(Effect.ignore);
  const grant = row.github.grant;
  if (grant && row.machineId) yield* endGrant(row.organizationId, row.machineId, grant.id).pipe(Effect.ignore);
}).pipe(Effect.catch((error) => Effect.logWarning("Could not cancel a GitHub build run.", error)));

/** Idempotent. Only the Machine that minted the grant knows it. */
const endGrant = (organizationId: string, machineId: MachineId, grantId: BuildGrantId) =>
  connectedRuntime(organizationId, machineId).pipe(
    Effect.flatMap((sdk) => sdk.endBuildGrant(grantId)),
    Effect.scoped,
  );

/** An ended attempt's GitHub builds: cancel each run and end its grant. Idempotent. */
export const cancelGithubImageBuilds = Effect.fn("Deployments.cancelGithubImageBuilds")(function* (inngestRunId: string) {
  const rows = yield* loadGithubImageBuilds(inngestRunId);
  yield* Effect.forEach(rows, cancelGithubBuildRun, { concurrency: 4, discard: true });
  return rows.length;
});
