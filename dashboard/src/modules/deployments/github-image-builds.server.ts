import "@tanstack/react-start/server-only";
import type { BuildGrantId, BuildReceipt, MachineId, PreparationEvent } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { cancelGithubRun, checkGithubBuildWorkflow, dispatchGithubBuildWorkflow, githubRunCompleted } from "#/modules/github/github-build.server";
import { verifyGithubOidcToken } from "#/modules/github/github-oidc.server";
import { buildFingerprints, buildGrantTag, ployzVersion } from "#/modules/runtime/ployz.server";
import { AppConfig } from "#/server/config.server";
import { BuildGrantUnavailable, Conflict, Forbidden, NotFound, Unauthorized, Validation } from "#/server/public-error";
import type { BuildCandidate } from "./build-order";
import { persistBuildLog } from "./deployment-events.server";
import {
  awaitsCheckIn, checkInImageBuild, claimForGithub, loadGithubImageBuilds, loadImageBuild, recordGithubReport, settleImageBuild, settled,
  skipImageBuilder, skipUnstarted, START_WITHIN_MINUTES,
  type ImageBuildAttempt, type ImageBuildRow, type ImageBuildTarget,
} from "./image-builds.server";
import { preparationProgressCollector, type BuildOutputWrite, type BuildStepWrite } from "./preparation-progress";
import { loadDeploymentContext } from "./runtime-hydration.repository.server";
import type { DeploymentContext } from "./runtime-repository.contract";
import { connectedRuntime, oneServiceDeployment } from "./runtime-session.server";
import { pinSourceCommit } from "./runtime-sources.server";

/**
 * GitHub as a Builder. Cloud dispatches the repository's build workflow, the runner checks in once
 * with its OIDC token for the build's grant and secrets, posts its Build Steps as it builds, and
 * when the run completes Cloud ends the grant and writes the receipt from the digest the Machine received.
 *
 *   dispatch ──▶ check-in (once) ──▶ steps … ──▶ workflow_run completed ──▶ end grant ──▶ receipt
 *
 * Check-in is the build starting. Until then GitHub can still be skipped: at once when it can't take
 * the build, at the "start within" limit, or when the run ends first.
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
 * Dispatches an Image Build to GitHub Actions on the native runner for its one platform, and records
 * why GitHub took it. GitHub is skipped at once, with the reason on the Image Build, when it can't
 * take the build: the repository isn't reachable through the GitHub App or lacks permission, has no
 * workflow, needs several platforms, or the dispatch fails.
 */
export const startGithubImageBuild = Effect.fn("Deployments.startGithubImageBuild")(function* (build: ImageBuildTarget, candidate: Pick<BuildCandidate, "reason">) {
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
  const platforms = yield* connectedRuntime(context.organization.id).pipe(Effect.flatMap((sdk) => sdk.buildPlatforms(deployment)), Effect.scoped);
  if (platforms.length > 1) {
    return yield* skipUnstarted(build, { builder: "github", kind: "multi_platform", platforms: platforms.map((platform) => platform.replace(/^linux\//, "")) });
  }
  // With no visible placement, deploy's coverage check decides, as it does after a server build.
  const runner = platforms[0] === "linux/arm64" ? "ubuntu-24.04-arm" : "ubuntu-latest";
  // The check-in hands the runner this pinned commit.
  yield* pinSourceCommit(context, snapshot, source);
  const config = yield* AppConfig;
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
  if (row?.status !== "building" || row.builder !== "github") return settled(build, row?.status ?? "failed") satisfies GithubBuildCheck;
  if (seen.ended || (yield* githubRunEnded(row))) return yield* finishGithubImageBuild(build, row, false);
  if (row.checkedInAt === null) return seen.startLimit ? yield* withdrawGithubImageBuild(build, row) : waiting;
  if (Date.now() - row.checkedInAt.getTime() > GITHUB_RUN_BUDGET_MS) return yield* finishGithubImageBuild(build, row, true);
  return waiting;
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
  }).pipe(Effect.scoped);
  const grant = { id: minted.id, fingerprint };
  if (!(yield* checkInImageBuild({ imageBuildId: row.id, runId: row.githubRunId, machineId: machine.id, grant }))) {
    // Lost to a second check-in or to the start-within skip during a slow mint (rare). The grant's
    // secret never left Cloud, so a grant that fails to end is unusable anyway.
    yield* endGrant(row.organizationId, machine.id, minted.id).pipe(
      Effect.catch((error) => Effect.logWarning("Could not end an unclaimed Build Grant.", error)),
    );
    return yield* refused;
  }
  // The runner installs this version: the process that computed the fingerprint names it, even mid-rollout.
  return { grant: minted.grant, commit, fingerprint, ployzVersion: ployzVersion(), deployment };
});

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
  if (platforms) steps.push(...collector.finish(platforms.length ? null : "The build failed on GitHub; see its run."));
  yield* persistBuildLog(row.deploymentId, { steps, output }, row.image);
  const taken = Math.max(received, report.from + report.events.length);
  if (!(yield* recordGithubReport(row.id, received, { received: taken, collector: collector.checkpoint(), platforms: platforms && [...platforms] }))) {
    return yield* new Conflict({ message: "Another report of this build was taken first." });
  }
  return { received: taken };
});

/**
 * Settles a GitHub build once its run ended, or ran out of budget (`timedOut`: its run is cancelled
 * first): ends the grant and writes the receipt from the digest the Machine verified. A run that
 * ended before it checked in never started, so GitHub is skipped. A started run that pushed nothing failed.
 */
const finishGithubImageBuild = Effect.fn("Deployments.finishGithubImageBuild")(function* (build: ImageBuildTarget, row: GithubRow, timedOut: boolean) {
  if (row.checkedInAt === null) {
    const skip = yield* skipImageBuilder(build, { builder: "github", kind: "ended_before_start" });
    // It checked in just now; the next check finds its run ended on GitHub and finishes it.
    return skip.kind === "started" ? waiting : skip;
  }
  if (timedOut) yield* cancelGithubBuildRun(row);
  const failed = (message: string) => settleImageBuild(build, { status: "failed", message, machineId: row.machineId });
  const grant = row.github.grant;
  if (!grant || !row.machineId) return yield* failed("GitHub: the run ended before it received its grant.");
  const pushed = yield* endGrant(row.organizationId, row.machineId, grant.id).pipe(
    Effect.map((ended) => ended.pushed ?? null),
    Effect.orElseSucceed(() => null),
  );
  const platforms = row.github.report?.platforms;
  if (!pushed || !platforms?.length) return yield* failed(timedOut ? "GitHub: the run didn't finish within 2 hours." : "GitHub: the run pushed no image.");
  const receipt: BuildReceipt = {
    fingerprint: grant.fingerprint, machine_id: row.machineId,
    image: { reference: pushed, tags: [buildGrantTag(grantRepository(row.image), pushed)], platforms: [...platforms], location: "build-grant" },
  };
  return yield* settleImageBuild(build, { status: "built", receipt });
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
