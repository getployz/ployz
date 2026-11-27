import "@tanstack/react-start/server-only";
import type { ConfigStore, GithubBuild, GithubClaims } from "@ployz/sdk";
import { eq } from "drizzle-orm";
import { Cause, Effect, Exit, Schedule, Schema } from "effect";
import { cloudStore, dialedStore, refusedWith, storeTry, withDeployDial } from "#/modules/config-store/store-sdk.server";
import { runnerRefusal } from "#/modules/config-store/store-refusal";
import { pinStoreSources, sourceIdentity } from "#/modules/config-store/store-sources.server";
import { GithubSources } from "#/modules/github/github-sources.server";
import { deploymentRun } from "#/modules/config-store/tables";
import { installationAccess, organizationAccess } from "#/modules/github/github-access";
import {
  cancelGithubRun, checkGithubBuildWorkflow, dispatchApprovedGithubBuild, dispatchGithubBuildWorkflow, findGithubPushRun, githubRunCompleted, rerunGithubRun,
} from "#/modules/github/github-build.server";
import { deploymentOfBuild, githubBuildId } from "#/modules/github/github-build-workflow";
import { verifyGithubOidcToken } from "#/modules/github/github-oidc.server";
import { GithubAuthority } from "#/modules/github/github-authority.server";
import { installationRefusal } from "#/modules/github/github-token.server";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createGithubBuildRunCompletedEvent } from "#/modules/github/github.events";
import type { ConfigDeploymentAdmittedEventData } from "#/modules/config-store/config-store.events";
import { requirements } from "#/modules/runtime/daemon-compat";
import { firstRuntimeFrame } from "#/modules/runtime/organization-runtime.server";
import { bearerToken } from "#/server/bearer";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { Conflict, Forbidden, NotFound, Unauthorized, Validation } from "#/server/public-error";
import { serversLackingEnsure, serversLackingIn } from "#/modules/runtime/daemon-compat.server";

/**
 * GitHub as a Builder for Store Deployments. Before the runner claims a Deployment, each Git Service whose walk
 * (Preferred Builder, then the Build Order, both from the Store) starts with GitHub goes to GitHub Actions: Cloud
 * checks the repository's workflow, dispatches it, and waits while the runner checks in with its OIDC token for its
 * Build Grant and build inputs, reports its Build Steps, and pushes. The Store records every step; Rust touches the
 * secrets and the Machines. The runner then delivers what GitHub built, and builds on the servers whatever GitHub
 * skipped when the walk has them next.
 *
 *   plan ─▶ start (reuse | skip | dispatch) ─▶ wait ⇄ check ─▶ built | failed | skipped ─▶ run-deployment
 */

/** How long a run may build after it checked in before Cloud cancels it; the daemon's grant lives 3h. */
export const GITHUB_RUN_BUDGET_MS = 2 * 60 * 60_000;
/** How often Cloud looks at a dispatched run between Workflow run webhooks. */
export const GITHUB_CHECK_INTERVAL = "10m";
/** How long GitHub has to start a build a later Builder could take. */
export const START_WITHIN_MINUTES = 3;

/**
 * Cancel a run Cloud dispatched, as Cloud itself through the installation that runs it: the Organization's own token
 * is refused once it disconnected. A run that already ended is fine. GitHub refuses every token of an installation it
 * suspended or uninstalled, so those runs end at the runner's next refused call to Cloud instead.
 */
export const cancelDispatchedGithubRun = (run: GithubBuild["run"]) =>
  cancelGithubRun({ access: installationAccess(run.installation_id), fullName: run.repository, runId: run.run_id }).pipe(
    Effect.catch((error) => Effect.logWarning("Could not cancel a GitHub run Cloud dispatched.", { runId: run.run_id, error })),
  );

/**
 * The build GitHub holds, or held until it ended; `null` once GitHub passed it to the next Builder.
 * A build's id is DEPLOYMENT.SERVICE.
 */
export const heldGithubBuild = Effect.fn("ConfigStore.heldGithubBuild")(function* (buildId: string) {
  const store = yield* cloudStore;
  const handOff = yield* storeTry(() => store.githubBuild(buildId));
  return "held" in handOff ? handOff.held : null;
});

/**
 * Stop a Deployment's builds still on GitHub: fail them and cancel their runs. Their Build Grants expire by
 * themselves. Idempotent. Its cancellation, and a worker that gave up on it, call it.
 */
export const cancelStoreGithubBuilds = Effect.fn("ConfigStore.cancelGithubBuilds")(function* (deploymentId: string) {
  const store = yield* cloudStore;
  const builds = yield* storeTry(() => store.githubCancel(deploymentId));
  yield* Effect.forEach(builds, (build) => cancelDispatchedGithubRun(build.run), { concurrency: 4, discard: true });
  return builds.length;
});

/**
 * Stop the GitHub builds that `organizationIds` run through `installationId` once they can't use it: disconnected,
 * suspended or uninstalled. Call it after that change is recorded. Each run is cancelled and its walk woken, so the
 * walk looks now rather than at its next interval, finds the change, and fails the Deployment with it. Idempotent: an
 * ended run answers 409, and the walk ignores a wake for a build it already failed.
 */
export const stopGithubInstallationBuilds = Effect.fn("ConfigStore.stopGithubInstallationBuilds")(function* (
  installationId: number, organizationIds: ReadonlyArray<string>,
) {
  const store = yield* cloudStore;
  const { drizzle } = yield* Database;
  const stopped: string[] = [];
  for (const organizationId of organizationIds) {
    const runs = yield* drizzle.select({ deploymentId: deploymentRun.deploymentId }).from(deploymentRun)
      .where(eq(deploymentRun.organizationId, organizationId));
    for (const { deploymentId } of runs) {
      // A Deployment or build the Store no longer has has no GitHub run to stop.
      const view = yield* storeTry(() => store.read(organizationId, { query: "deployment", id: deploymentId })).pipe(
        Effect.catchIf(refusedWith("not_found"), () => Effect.succeed(null)));
      for (const build of view?.builds ?? []) {
        if (build.status !== "building") continue;
        const github = yield* heldGithubBuild(`${deploymentId}.${build.service}`).pipe(
          Effect.catchIf(refusedWith("not_found"), () => Effect.succeed(null)));
        if (github === null || github.status !== "building" || github.run.installation_id !== installationId) continue;
        yield* cancelDispatchedGithubRun(github.run);
        yield* sendInngestEvent(createGithubBuildRunCompletedEvent({ id: `stopped-${github.id}`, runId: github.run.run_id }));
        stopped.push(github.id);
      }
    }
  }
  return stopped;
});

/** One Git build the walk hands to GitHub first. No secret: it is a step's output. */
export type StoreGithubTarget = {
  build: string;
  service: string;
  repository: string;
  repositoryId: number;
  installationId: number | null;
  /** Another Builder follows in its walk, so GitHub gets START_WITHIN_MINUTES to start it. */
  hasNext: boolean;
};

/**
 * Whether any Server takes builds now, as the Cluster reports it. No answer reads as yes: the runner then says why the
 * Servers couldn't build.
 */
const serversBuild = (organizationId: string) => firstRuntimeFrame(organizationId).pipe(
  Effect.map((frame) => frame === null || frame.machines.some(({ machine }) => machine.accepts_builds)),
  Effect.orElseSucceed(() => true),
);

/**
 * The Servers whose records say they can't read a Build Grant back. Any Server may mint the grant, so one such Server
 * is enough to send the build to its next Builder. No answer names none: the build is attempted.
 */
const serversLackingGrants = (organizationId: string) => serversLackingIn(organizationId, requirements.buildGrantInspect);

const pinAndPrefetch = Effect.fn("StoreGithub.pinAndPrefetch")(function* (store: ConfigStore, data: ConfigDeploymentAdmittedEventData) {
  const sources = yield* pinStoreSources(store, data.organizationId, data.deploymentId).pipe(
    Effect.catchTag("SourceUnreadable", () => Effect.succeed([])),
    Effect.catchIf(refusedWith("conflict"), () => Effect.succeed([])),
  );
  const githubSources = yield* GithubSources;
  for (const source of sources) {
    if (source.commit !== null) yield* githubSources.prefetch({ ...sourceIdentity(data.organizationId, source), sha: source.commit });
  }
  return sources;
});

/**
 * The Servers too old to run a Deployment, read while its sources are pinned and start fetching. A pin never moves,
 * so planning reads the same pins after.
 */
export const serversLackingWhilePrefetching = Effect.fn("StoreGithub.serversLackingWhilePrefetching")(function* (data: ConfigDeploymentAdmittedEventData) {
  const store = yield* cloudStore;
  const [lacking] = yield* Effect.all([serversLackingEnsure(data.organizationId), pinAndPrefetch(store, data).pipe(Effect.ignore)], { concurrency: 2 });
  return lacking;
});

/**
 * Pin the Deployment's Git Services and list those whose walk starts with GitHub and haven't started; a walk that
 * starts with the Servers starts with its next Builder when no Server takes builds. Anything else (an unreadable
 * source, a Deployment no longer wanted) is left to the runner, which records it.
 */
export const planStoreGithubBuilds = Effect.fn("StoreGithub.plan")(function* (data: ConfigDeploymentAdmittedEventData) {
  const store = yield* cloudStore;
  const sources = yield* pinAndPrefetch(store, data);
  // A build GitHub skipped (with why) belongs to the next Builder.
  const pending = sources.filter((source) => source.status === "pending" && source.message === null);
  const servers = pending.some((source) => source.builders[0] === "servers" && source.builders.includes("github"))
    ? yield* serversBuild(data.organizationId)
    : true;
  return pending.flatMap((source): StoreGithubTarget[] => {
    const walk = servers ? source.builders : source.builders.filter((builder) => builder !== "servers");
    if (walk[0] !== "github") return [];
    return [{
      build: githubBuildId(data.deploymentId, source.service),
      service: source.service,
      repository: source.repository,
      repositoryId: source.repository_id,
      installationId: source.access.type === "github-installation" ? source.access.installationId : null,
      hasNext: walk.length > 1,
    }];
  });
});

export type StoreGithubStart = { kind: "dispatched"; runId: number } | { kind: "done" };
type GithubRun = Parameters<ConfigStore["githubDispatched"]>[1];
/**
 * What a build's start leaves to record: the run GitHub started, or a skip when GitHub gave no usable answer or can't
 * take the build. GitHub may have started a run either way, so a record the Store refused is retried on its own, and
 * GitHub is never asked again.
 */
/**
 * How a build started, for the Store to record: the run that took it, or why it skipped GitHub. `dispatched` marks a run
 * Cloud asked GitHub for, which is cancelled if its record is lost; a push's own run is only followed.
 */
export type StoreGithubRecord = { kind: "run"; build: string; run: GithubRun; dispatched: boolean } | { kind: "skip"; build: string; message: string };
type StoreGithubDispatch = StoreGithubStart | StoreGithubRecord;
/**
 * How far a start got through its builds, in order: how each of the first builds started, then the record the Store
 * refused, if any, and the builds after it, which haven't started.
 */
export type StoreGithubStarts = { started: StoreGithubStart[]; unrecorded: StoreGithubRecord | null; rest: StoreGithubTarget[] };
/** The one request to GitHub a build's start makes, once everything before it has settled. */
type GithubAsk = { build: string; installationId: number; repositoryId: number; repository: string } & (
  | { kind: "rerun"; run: GithubRun }
  | { kind: "approved"; branch: string; workflow: string; commit: string; workflowRef: string }
  | { kind: "dispatch"; branch: string; runner: string }
);

type WorkflowRead = ReturnType<typeof checkGithubBuildWorkflow>;
/** What starting a build reads beyond the build itself, the same for every build in a Deployment. */
type StartReads = {
  readonly workflow: (access: Parameters<typeof checkGithubBuildWorkflow>[0], repositoryId: number) => WorkflowRead;
  readonly lacking: ReturnType<typeof serversLackingGrants>;
};

/** Every effect run to its end, then the failures of all that failed, so one failing never cuts another off. */
const settleAll = <A, E, R>(effects: ReadonlyArray<Effect.Effect<A, E, R>>) => Effect.gen(function* () {
  const exits = yield* Effect.forEach(effects, Effect.exit, { concurrency: "unbounded" });
  const [first, ...rest] = exits.filter(Exit.isFailure);
  if (first !== undefined) return yield* Effect.failCause(rest.reduce((cause, failure) => Cause.combine(cause, failure.cause), first.cause));
  return exits.filter(Exit.isSuccess).map((exit) => exit.value);
});

/**
 * Start a Deployment's GitHub builds, reading each repository's workflow once. Only a build still waiting for its first
 * Builder starts. One a run already took is followed, and one that ended or was skipped is left alone. Every read
 * settles first, so a failed read fails before GitHub is asked for anything. Then each build is asked for and recorded
 * before the next is asked for, so a retry, even one after the answer was lost, follows every run GitHub started. A
 * record the Store refuses ends the start there: GitHub may have taken that build, so it is never asked for again.
 */
export const startStoreGithubBuilds = Effect.fn("StoreGithub.startAll")(function* (
  organizationId: string, deploymentId: string, targets: ReadonlyArray<StoreGithubTarget>,
) {
  const store = yield* cloudStore;
  const sources = yield* storeTry(() => store.deploymentSources(deploymentId));
  const waiting = new Set(sources.filter((source) => source.status === "pending" && source.message === null).map((source) => source.service));
  // A repository reaches the Organization through one installation.
  const workflows = new Map<number, WorkflowRead>();
  for (const { installationId, repositoryId } of targets) {
    if (installationId === null || workflows.has(repositoryId)) continue;
    workflows.set(repositoryId, yield* Effect.cached(checkGithubBuildWorkflow(organizationAccess(organizationId, installationId), repositoryId)));
  }
  const reads: StartReads = {
    workflow: (access, repositoryId) => workflows.get(repositoryId) ?? checkGithubBuildWorkflow(access, repositoryId),
    lacking: yield* Effect.cached(serversLackingGrants(organizationId)),
  };
  const prepared = yield* settleAll(targets.map((target) => prepareStoreGithubBuild(organizationId, target, reads, waiting.has(target.service))));
  const started: StoreGithubStart[] = [];
  for (const [index, next] of prepared.entries()) {
    const dispatch = "installationId" in next ? yield* askGithub(organizationId, next) : next;
    const recorded = yield* Effect.exit(recordStoreGithubBuild(dispatch));
    if (Exit.isFailure(recorded)) {
      if (dispatch.kind === "dispatched" || dispatch.kind === "done") return yield* Effect.failCause(recorded.cause);
      return { started, unrecorded: dispatch, rest: targets.slice(index + 1) } satisfies StoreGithubStarts;
    }
    started.push(recorded.value);
  }
  return { started, unrecorded: null, rest: [] } satisfies StoreGithubStarts;
});
const done = { kind: "done" } as const;

/** The run that holds the build, to follow. A build no run has held yet is not found. */
const heldRun = (build: string) => heldGithubBuild(build).pipe(
  Effect.catchIf(refusedWith("not_found"), () => Effect.succeed(null)),
  Effect.map((held) => held?.status === "building" ? held.run.run_id : null),
);
const following = (runId: number | null): StoreGithubStart => runId === null ? done : { kind: "dispatched", runId };

/**
 * Decide how a build starts on GitHub, reading but never asking GitHub to run anything: reuse the Service's latest
 * image when a Machine still holds it for this commit, skip GitHub when it can't take it (no GitHub App access, no
 * build workflow, several platforms, GitHub or the Cluster unreachable), else the run to ask for, on the runner native
 * to its platform. Workflow inputs are visible on GitHub, so they never carry secrets: the runner fetches those at
 * check-in. A build a run already holds is followed, not started again: no other Builder may take it until that run
 * lets it go.
 */
const prepareStoreGithubBuild = Effect.fn("StoreGithub.prepare")(function* (
  organizationId: string, target: StoreGithubTarget, reads: StartReads, waiting: boolean,
) {
  const held = yield* heldRun(target.build);
  if (held !== null || !waiting) return following(held);
  // The build started or ended since the read above: follow whichever run holds it now.
  const refused = heldRun(target.build).pipe(Effect.map(following));
  const skip = (message: string) => skipped(target.build, message);
  const store = yield* cloudStore;
  if (target.installationId === null) {
    // A Service whose image a Server still holds reads as reused, not as waiting on an App it doesn't need.
    const start = yield* withDeployDial(organizationId, ({ connections, expect }) => store.githubStart(target.build, connections, expect)).pipe(
      Effect.catchIf(refusedWith("conflict"), () => Effect.succeed(null)),
    );
    if (start === null) return yield* refused;
    if (start.kind === "reused") return done;
    return skip(`GitHub builds need the Ployz GitHub App on ${target.repository}`);
  }
  const installationId = target.installationId;
  const access = organizationAccess(organizationId, installationId);
  return yield* Effect.gen(function* () {
    const workflow = yield* reads.workflow(access, target.repositoryId);
    const repository = workflow.fullName ?? target.repository;
    if (workflow.readiness === "no_permission") return skip(`the Ployz GitHub App can't run Actions in ${repository}`);
    if (workflow.readiness === "setup_needed" || !workflow.fullName || !workflow.defaultBranch) {
      return skip(`${repository} has no .github/workflows/ployz-build.yml on its default branch`);
    }
    const lacking = yield* reads.lacking;
    if (lacking.length > 0) return skip(`GitHub builds need a newer Ployz on ${lacking.join(", ")}`);
    const start = yield* withDeployDial(organizationId, ({ connections, expect }) => store.githubStart(target.build, connections, expect));
    const ask = { build: target.build, installationId, repositoryId: target.repositoryId, repository: workflow.fullName };
    if (start.kind === "follow") {
      // A push under an automatic deploy rule started its own run: follow it rather than dispatch one.
      const run = yield* followPushRun({ access, repositoryId: target.repositoryId, workflow: start.workflow, commit: start.commit });
      if (run === null) return skip(`GitHub started no run of ${start.workflow} for this push`);
      const recorded: GithubRun = {
        run_id: run.runId, run_url: run.runUrl, workflow_ref: start.workflow_ref, repository: workflow.fullName, installation_id: installationId,
      };
      // A run that ended before Cloud followed it gave up waiting at its check-in: run it again.
      if (run.completed) return { ...ask, kind: "rerun", run: recorded } satisfies GithubAsk;
      return { kind: "run", build: target.build, run: recorded, dispatched: false } satisfies StoreGithubRecord;
    }
    if (start.kind === "approved") {
      // A person's Deploy under a rule builds on the rule's workflow, on its branch, at the commit it pinned; the grant
      // Machine builds it only while it is that branch's head.
      return { ...ask, kind: "approved", branch: start.branch, workflow: start.workflow, commit: start.commit, workflowRef: start.workflow_ref } satisfies GithubAsk;
    }
    if (start.kind !== "dispatch") return done;
    // A rule build runs the rule's own template, whose builder the grant Machine checks. Through the minter, Cloud
    // dispatches only its own.
    // The rule template takes no build input, so GitHub would refuse this dispatch.
    if (workflow.template === "rule") return skip(`${repository}'s .github/workflows/ployz-build.yml builds only under its rules`);
    if (!workflow.dispatchable) return skip(`${repository}'s .github/workflows/ployz-build.yml isn't the one Ployz offers`);
    return { ...ask, kind: "dispatch", branch: workflow.defaultBranch, runner: start.runner } satisfies GithubAsk;
  }).pipe(
    // GitHub failing before dispatch is GitHub being unusable, not the build failing.
    Effect.catchTag("GithubObservationError", (error) => Effect.succeed(skip(`GitHub couldn't take the build: ${error.message}`))),
    Effect.catchTag("GithubDisconnected", (error) => Effect.succeed(skip(error.message))),
    Effect.catchIf(refusedWith("conflict"), () => refused),
  );
});

/**
 * Ask GitHub for the build's run. This never fails: GitHub may have taken a request it never answered, so any failure
 * reads as GitHub being unusable, and the build goes to its next Builder rather than being asked for again.
 */
const askGithub = (organizationId: string, ask: GithubAsk) => Effect.gen(function* () {
  const access = organizationAccess(organizationId, ask.installationId);
  const answer = (runId: number, runUrl: string, workflowRef: string): GithubRun =>
    ({ run_id: runId, run_url: runUrl, workflow_ref: workflowRef, repository: ask.repository, installation_id: ask.installationId });
  if (ask.kind === "rerun") {
    yield* rerunGithubRun({ access, repositoryId: ask.repositoryId, runId: ask.run.run_id });
    return ask.run;
  }
  if (ask.kind === "approved") {
    const run = yield* dispatchApprovedGithubBuild({ access, repositoryId: ask.repositoryId, branch: ask.branch, workflow: ask.workflow, commit: ask.commit });
    return answer(run.runId, run.runUrl, ask.workflowRef);
  }
  const run = yield* dispatchGithubBuildWorkflow({ access, repositoryId: ask.repositoryId, branch: ask.branch, inputs: { build: ask.build, runner: ask.runner } });
  return answer(run.runId, run.runUrl, run.workflowRef);
}).pipe(
  Effect.map((run): StoreGithubRecord => ({ kind: "run", build: ask.build, run, dispatched: ask.kind !== "rerun" })),
  Effect.catchTag("GithubObservationError", (error) => Effect.succeed(skipped(ask.build, `GitHub couldn't take the build: ${error.message}`))),
  Effect.catchTag("GithubDisconnected", (error) => Effect.succeed(skipped(ask.build, error.message))),
  Effect.catchCause(() => Effect.succeed(skipped(ask.build, "GitHub couldn't take the build"))),
);
const skipped = (build: string, message: string): StoreGithubRecord => ({ kind: "skip", build, message });

/**
 * Record how a build started, retrying while the Store is unavailable. A run Cloud dispatched that the Store refused
 * because the build started or ended meanwhile is cancelled, unless it is the run that holds the build, as when an
 * earlier try's record committed late. A push's own run is only followed, and never cancelled. On the `last` try, a
 * dispatched run whose record still fails is given up: see `giveUpRun`.
 */
export const recordStoreGithubBuild = Effect.fn("StoreGithub.record")(function* (dispatch: StoreGithubDispatch, last = false) {
  if (dispatch.kind === "dispatched" || dispatch.kind === "done") return dispatch;
  const store = yield* cloudStore;
  const record = dispatch.kind === "skip"
    ? storeTry(() => store.githubSkip(dispatch.build, dispatch.message)).pipe(Effect.as<StoreGithubStart>(done))
    : storeTry(() => store.githubDispatched(dispatch.build, dispatch.run)).pipe(Effect.as<StoreGithubStart>({ kind: "dispatched", runId: dispatch.run.run_id }));
  const ours = dispatch.kind === "run" && dispatch.dispatched ? dispatch.run : null;
  return yield* record.pipe(
    Effect.retry({ while: refusedWith("unavailable"), times: 5, schedule: Schedule.exponential("1 second") }),
    Effect.catchIf(refusedWith("conflict"), () => Effect.gen(function* () {
      const runId = yield* heldRun(dispatch.build);
      if (ours !== null && runId !== ours.run_id) yield* cancelDispatchedGithubRun(ours);
      return following(runId);
    })),
    Effect.onError(() => last && ours !== null ? giveUpRun(dispatch.build, ours) : Effect.void),
  );
});

/**
 * Cancel a dispatched run whose record failed on the last try, once the Store proves it can never hold the build:
 * failure cleanup stops only runs the Store recorded. A timed-out record may still commit, so the build is first
 * passed on, which refuses any later record of a run. If that pass-on is refused, the run is cancelled only when it
 * isn't the holder; if the Store doesn't answer, the run is left alone.
 */
const giveUpRun = (build: string, run: GithubRun) => Effect.gen(function* () {
  const store = yield* cloudStore;
  const passedOn = yield* storeTry(() => store.githubSkip(build, "GitHub's run couldn't be recorded")).pipe(
    Effect.as(true),
    Effect.catchIf(refusedWith("conflict"), () => heldRun(build).pipe(Effect.map((held) => held !== run.run_id))),
  );
  if (passedOn) yield* cancelDispatchedGithubRun(run);
}).pipe(Effect.ignore);

/** How long Cloud waits for GitHub to list the run a push started, and how often it looks. */
const PUSH_RUN_WAIT_MS = 90_000;
const PUSH_RUN_LOOK = "3 seconds";

const followPushRun = Effect.fn("StoreGithub.followPushRun")(function* (input: Parameters<typeof findGithubPushRun>[0]) {
  const deadline = Date.now() + PUSH_RUN_WAIT_MS;
  for (;;) {
    const run = yield* findGithubPushRun(input);
    if (run !== null || Date.now() >= deadline) return run;
    yield* Effect.sleep(PUSH_RUN_LOOK);
  }
});

/** Whether GitHub says the run completed; GitHub unreachable reads as still running. */
const runEnded = (build: GithubBuild) => githubRunCompleted({
  access: organizationAccess(build.organization, build.run.installation_id), fullName: build.run.repository, runId: build.run.run_id,
}).pipe(Effect.orElseSucceed(() => false));


/**
 * One look at a dispatched build, after its run completed (the webhook), its final report came, or a wait timed out.
 * A Deployment cancelled or replaced meanwhile stops it. An Organization that can't use the installation any more
 * (disconnected, or GitHub suspended or uninstalled it) can't read what GitHub built, so its builds stop and the
 * Deployment fails with why. A run past its budget is cancelled and ended. A run that hasn't checked in by the start
 * limit (`startLimit`: another Builder follows) is skipped and cancelled; GitHub keeps one whose check-in committed.
 */
export const checkStoreGithubBuild = Effect.fn("StoreGithub.check")(function* (
  organizationId: string, target: StoreGithubTarget, seen: { ended: boolean; startLimit: boolean },
) {
  const store = yield* cloudStore;
  const build = yield* heldGithubBuild(target.build);
  if (build === null || build.status !== "building") return "done" as const;
  const deploymentId = deploymentOfBuild(build.id);
  const view = yield* storeTry(() => store.read(organizationId, { query: "deployment", id: deploymentId }));
  if (view.status !== "queued" && view.status !== "running") {
    yield* cancelStoreGithubBuilds(deploymentId);
    return "done" as const;
  }
  const refusal = yield* installationRefusal(organizationId, build.run.installation_id);
  if (refusal !== null) {
    yield* cancelStoreGithubBuilds(deploymentId);
    return { failed: refusal.message };
  }
  const finish = (look: "start_limit" | "budget" | "ended") => Effect.gen(function* () {
    const finished = yield* dialedStore(organizationId, ({ connections, expect }) => store.githubFinish(target.build, look, connections, expect));
    return finished === "waiting" ? "waiting" as const : "done" as const;
  });
  if (build.checked_in_at !== null && Date.now() - build.checked_in_at * 1000 > GITHUB_RUN_BUDGET_MS) {
    yield* cancelDispatchedGithubRun(build.run);
    return yield* finish("budget");
  }
  if (build.grant === null) {
    if (seen.ended || (yield* runEnded(build))) return yield* finish("ended");
    if (!seen.startLimit) return "waiting" as const;
    const found = yield* finish("start_limit");
    if (found === "done") yield* cancelDispatchedGithubRun(build.run);
    return found;
  }
  if (seen.ended || build.ended !== null || (yield* runEnded(build))) return yield* finish("ended");
  return "waiting" as const;
});

/** Whether a build GitHub holds is still on GitHub: a final report may settle it before a wait begins. */
export const storeGithubBuildOpen = Effect.fn("StoreGithub.open")(function* (target: StoreGithubTarget) {
  return (yield* heldGithubBuild(target.build))?.status === "building";
});

/** The runner's verified OIDC claims; the Store checks them against the build's repository, workflow and run. */
const runnerClaims = Effect.fn("StoreGithub.claims")(function* (request: Request) {
  const config = yield* AppConfig;
  const token = bearerToken(request.headers);
  if (!token) return yield* new Unauthorized();
  const claims = yield* verifyGithubOidcToken(token, config.app.url.origin).pipe(Effect.mapError(() => new Unauthorized()));
  return { repository_id: claims.repository_id, job_workflow_ref: claims.job_workflow_ref, run_id: claims.run_id, event_name: claims.event_name } satisfies GithubClaims;
});

const passedOn = () => new Conflict({ message: "GitHub passed this build to the next Builder." });

/**
 * The build a runner's call is about, once GitHub still holds it (else the next Builder has it: `Conflict`) and its
 * Organization can use the installation that runs it (else `Forbidden`, saying why).
 */
const guardHeldBuild = Effect.fn("StoreGithub.guardHeld")(function* (held: GithubBuild | null) {
  if (held === null) return yield* passedOn();
  const refusal = yield* installationRefusal(held.organization, held.run.installation_id);
  if (refusal !== null) return yield* new Forbidden({ message: refusal.message });
  return held;
});

const MAX_CHECK_IN_BYTES = 4 * 1024;
const CheckIn = Schema.fromJsonString(Schema.Struct({
  holder: Schema.optional(Schema.NullOr(Schema.String.check(Schema.isPattern(/^[0-9a-f]{64}$/)))),
}));
const notCheckIn = () => new Validation({ message: "The check-in is not a holder key." });

/** The public key the runner holds its Build Grant with: none from an Action that sends no body. */
const holderOf = Effect.fn("StoreGithub.holder")(function* (text: string) {
  if (text.length > MAX_CHECK_IN_BYTES) return yield* notCheckIn();
  if (text.trim() === "") return null;
  const { holder } = yield* Schema.decodeUnknownEffect(CheckIn)(text).pipe(Effect.mapError(notCheckIn));
  return holder ?? null;
});

/**
 * A runner's one check-in for a Store build: how it pushes, its pinned commit, expected fingerprint, the ployz
 * version that computed it, and the one-Service deployment whose variables carry the build secrets.
 */
export const checkInStoreGithubBuild = Effect.fn("StoreGithub.checkIn")(function* (request: Request, buildId: string, text: string) {
  const claims = yield* runnerClaims(request);
  const holder = yield* holderOf(text);
  const store = yield* cloudStore;
  return yield* Effect.gen(function* () {
    const build = yield* guardHeldBuild(yield* heldGithubBuild(buildId));
    const { foreign, ...checkIn } = yield* withDeployDial(build.organization, ({ connections, expect }) =>
      store.githubCheckIn(buildId, claims, holder, connections, expect));
    // Only what the runner pushes and builds with goes back to it.
    return checkIn;
  }).pipe(Effect.catchTag("StoreRefused", (error) => Effect.fail(runnerRefusal(error))));
});

const MAX_STEPS_REPORT_BYTES = 16 * 1024 * 1024;

/**
 * A runner's batch of Build Steps for a Store build, into its build log. The final one (with `platforms`) ends the
 * build at once, its grant ended and its receipt written, and wakes the walk.
 */
export const recordStoreGithubBuildSteps = Effect.fn("StoreGithub.steps")(function* (request: Request, buildId: string, text: string) {
  const claims = yield* runnerClaims(request);
  if (text.length > MAX_STEPS_REPORT_BYTES) return yield* new Validation({ message: "The report is too large." });
  const report: unknown = yield* Effect.try({ try: () => JSON.parse(text), catch: () => new Validation({ message: "The report is not Build Steps." }) });
  const store = yield* cloudStore;
  return yield* Effect.gen(function* () {
    const build = yield* guardHeldBuild(yield* heldGithubBuild(buildId));
    const reported = yield* storeTry(() => store.githubReport(buildId, claims, report));
    if (reported.ended) {
      // The walk's next look ends it otherwise.
      yield* dialedStore(build.organization, ({ connections, expect }) => store.githubFinish(buildId, "ended", connections, expect)).pipe(
        Effect.catch((error) => Effect.logWarning("Could not end a reported GitHub build.", error)),
      );
      yield* sendInngestEvent(createGithubBuildRunCompletedEvent({ id: `reported-${buildId}`, runId: build.run.run_id })).pipe(
        Effect.catch((error) => Effect.logWarning("Could not wake the walk for a reported GitHub build.", error)),
      );
    }
    return { received: reported.received };
  }).pipe(Effect.catchTag("StoreRefused", (error) => Effect.fail(runnerRefusal(error))));
});

/** How long a rule build's check-in waits for Cloud to follow its run: the push's webhook and the run race. */
const RULE_CHECK_IN_WAIT_MS = 60_000;
const RULE_CHECK_IN_LOOK = "2 seconds";

const RuleCheckInBody = Schema.fromJsonString(Schema.Struct({ commit: Schema.String.check(Schema.isPattern(/^[0-9a-f]{40}$/)) }));

/** The build the runner's run holds, once Cloud follows that run. */
const ruleBuildOf = Effect.fn("StoreGithub.ruleBuild")(function* (claims: GithubClaims) {
  const runId = Number(claims.run_id);
  if (!Number.isSafeInteger(runId)) return yield* new Unauthorized();
  const store = yield* cloudStore;
  const deadline = Date.now() + RULE_CHECK_IN_WAIT_MS;
  for (;;) {
    const id = yield* storeTry(() => store.githubByRun(runId));
    if (id !== null) {
      const build = yield* heldGithubBuild(id);
      if (build !== null) return { id, build };
    }
    if (Date.now() >= deadline) return yield* new NotFound({ message: "No build follows this run." });
    yield* Effect.sleep(RULE_CHECK_IN_LOOK);
  }
});

/**
 * A rule build's check-in, on the run its push started: the rule, the Cluster's keys and its one-Service deployment,
 * Sealed values sealed. Cloud relays them; the runner and the grant Machine check each.
 */
export const checkInRuleBuild = Effect.fn("StoreGithub.ruleCheckIn")(function* (request: Request, text: string) {
  const claims = yield* runnerClaims(request);
  if (text.length > MAX_CHECK_IN_BYTES) return yield* new Validation({ message: "The check-in is not a commit." });
  const { commit } = yield* Schema.decodeUnknownEffect(RuleCheckInBody)(text).pipe(
    Effect.mapError(() => new Validation({ message: "The check-in is not a commit." })),
  );
  const store = yield* cloudStore;
  return yield* Effect.gen(function* () {
    const build = yield* guardHeldBuild((yield* ruleBuildOf(claims)).build);
    const { foreign, ...checkIn } = yield* withDeployDial(build.organization, ({ connections, expect }) =>
      store.githubRuleCheckIn(claims, commit, connections, expect));
    return checkIn;
  }).pipe(Effect.catchTag("StoreRefused", (error) => Effect.fail(runnerRefusal(error))));
});

const MAX_GRANT_ASK_BYTES = 16 * 1024;

/**
 * A rule build's grant: Cloud relays the runner's ask to a Machine with the build's Sealed values, still sealed, and a
 * token that only reads the repository, for the Machine's own read of the branch head.
 */
export const grantRuleBuild = Effect.fn("StoreGithub.ruleGrant")(function* (request: Request, buildId: string, text: string) {
  const claims = yield* runnerClaims(request);
  if (text.length > MAX_GRANT_ASK_BYTES) return yield* new Validation({ message: "The grant ask is too large." });
  const ask: unknown = yield* Effect.try({ try: () => JSON.parse(text), catch: () => new Validation({ message: "The grant ask is not JSON." }) });
  const store = yield* cloudStore;
  return yield* Effect.gen(function* () {
    const build = yield* guardHeldBuild(yield* heldGithubBuild(buildId));
    const repositoryId = Number(claims.repository_id);
    const reader = yield* (yield* GithubAuthority).readToken(build.run.installation_id, { repositoryIds: [repositoryId], permissions: { contents: "read" } }).pipe(
      Effect.catch((error) => Effect.logWarning("Could not mint a head reader for a rule build.", error).pipe(Effect.as(null))),
    );
    const { machine } = yield* withDeployDial(build.organization, ({ connections, expect }) =>
      store.githubRuleGrant(claims, ask, reader, connections, expect));
    return { machine };
  }).pipe(Effect.catchTag("StoreRefused", (error) => Effect.fail(runnerRefusal(error))));
});
