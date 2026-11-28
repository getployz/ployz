import { Effect } from "effect";
import {
  beginStoreDeploymentRun, deployToFirstServer, failStoreDeploymentUnrun, forgetStoreDeploymentRun, publishedEnvironments, recordStoreDeploymentRun, runStoreDeployment, type StoreServices,
  staleStoreDeploymentRuns, storeDeploymentRunner, storeDeploymentRunOf, unclaimedStoreDeployments,
} from "#/modules/config-store/store-deployment.server";
import { mailGithubDeployTrouble } from "#/modules/config-store/deploy-failed-mail.server";
import { deploymentTrouble } from "#/modules/config-store/store-deployments";
import { inngestRunStatus, type PloyzInngest } from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import { githubBuildRunCompleted } from "#/modules/github/github.events";
import { inngestFunctionCancelledEnvelopeSchema, inngestFunctionCancelledEventType, inngestFunctionFinishedEventType } from "#/modules/inngest/events";
import { type ConfigDeploymentAdmittedEventData, configDeploymentAdmitted, configDeploymentStopRequested, configFirstServerJoined, createConfigDeploymentStartedEvent } from "#/modules/config-store/config-store.events";
import { stopRun } from "#/modules/config-store/store-deployment-stop.inngest";
import { clusterDomainSyncRequested } from "#/modules/cluster-domain/cluster-domain.events";
import {
  cancelOwedGithubRuns, cancelStoreGithubBuilds, checkStoreGithubBuild, newerAttempt, GITHUB_CHECK_INTERVAL, planStoreGithubBuilds, START_WITHIN_MINUTES,
  recordStoreGithubBuild, serversLackingWhilePrefetching, startStoreGithubBuilds, storeGithubBuildOpen, type StoreGithubStart, type StoreGithubTarget,
} from "#/modules/config-store/store-github-builds.server";
import { followUp } from "#/modules/config-store/store-github.inngest";
import { sweepStore } from "#/modules/config-store/store-pull-request.server";
import { refreshMirrors } from "#/modules/volume-copies/mirror-sync/mirror-sync.round.server";
import { advancePlacement, type PlacementAdvance, type PlacementWait } from "#/modules/volume-placement/volume-placement.deploy.server";
import { syncUptimeChecks } from "#/modules/uptime/uptime-sync.server";
import { fenceDeployVolume, finishDeployVolumes, placeDeployVolumes, releaseFollowed } from "#/modules/volume-run/volume-run.deploy.server";
import { finishedVolumeRun } from "#/modules/volume-run/volume-run.inngest";
import { cleanOrphans } from "#/modules/volume-run/volume-run.orphans.server";
import { closeVolumeRun } from "#/modules/volume-run/volume-run.rows.server";
import { type EffectRunner, runInngestEffect } from "#/modules/inngest/run.server";

export const RUN_STORE_DEPLOYMENT_FUNCTION_ID = "run-store-deployment";
/** A step's tries past its first. Each step gets them anew. */
const STORE_DEPLOYMENT_RETRIES = 2;

/**
 * Cloud's worker runs each admitted Config Store Deployment: one step claims, prepares, confirms and records it in
 * Rust, so no secret, session or raw SDK evidence is ever a step's input or output. Deployments of one Environment
 * run one at a time; the Store supersedes the one still queued when a newer one is admitted. The run is recorded
 * first, so its cancellation finds the Deployment; a run that fails or is cancelled stops its GitHub builds and the
 * Store records that it stopped.
 */
export const createRunStoreDeployment = (inngest: PloyzInngest, runEffect: EffectRunner<StoreServices> = runInngestEffect) =>
  inngest.createFunction(
    {
      id: RUN_STORE_DEPLOYMENT_FUNCTION_ID,
      // A retried step claims again: before it prepared, it runs; after, the Store records the outcome unknown.
      retries: STORE_DEPLOYMENT_RETRIES,
      triggers: [{ event: configDeploymentAdmitted.eventType }],
      concurrency: [{ key: "event.data.environmentId", limit: 1 }],
      onFailure: async ({ event, step }) => {
        const { organizationId, deploymentId } = event.data.event.data;
        await runEffect(finishDeployVolumes(event.data.run_id, "failed"));
        const stopped = await stopRun(step, runEffect, { organizationId, deploymentId, runId: event.data.run_id, round: 0 });
        await runEffect(mailGithubDeployTrouble(event.data.event.data));
        return stopped;
      },
    },
    async ({ event, step, runId, attempt, maxAttempts }) => {
      await step.run("record-run", () => runEffect(recordStoreDeploymentRun(runId, event.data)));
      // Before it prepares anything: a later run of the same Deployment outranks this one, which then runs nothing.
      if (!(await step.run("begin", () => runEffect(beginStoreDeploymentRun(event.data.deploymentId, runId))))) {
        await step.run("forget-run", () => runEffect(forgetStoreDeploymentRun(runId)));
        return { outranked: event.data.deploymentId };
      }
      const runner = storeDeploymentRunner(runId);
      const lacking = await step.run("servers-lacking", () => runEffect(serversLackingWhilePrefetching(event.data, runner)));
      if (lacking.length > 0) {
        const failure = `Deploys need a newer Ployz on ${lacking.join(", ")}`;
        const failed = await step.run("servers-failed", () => runEffect(failStoreDeploymentUnrun(event.data.organizationId, event.data.environmentId, event.data.deploymentId, runId, failure)));
        await step.run("mail-failure", () => runEffect(mailGithubDeployTrouble(event.data)));
        return failed;
      }
      const github = await step.run("plan-builds", () => runEffect(planStoreGithubBuilds(event.data, runner)));
      const started = await startGithubBuilds(step, runEffect, event.data, github, runner, attempt + 1 >= (maxAttempts ?? STORE_DEPLOYMENT_RETRIES + 1));
      const walked = await Promise.all(github.map((target, index) => walkGithub(event.data.organizationId, target, started[index], runner, step, runEffect)));
      const disconnected = walked.find((failed) => failed !== null);
      if (disconnected !== undefined) {
        const failed = await step.run("github-failed", () => runEffect(failStoreDeploymentUnrun(event.data.organizationId, event.data.environmentId, event.data.deploymentId, runId, disconnected, undefined, true)));
        // Failed, the Deployment takes no more runs, so the Store ends what GitHub still holds and owes each run a
        // cancel. The run is forgotten only once that settled, or once the Store says a newer attempt holds it.
        await step.run("github-stop", () => runEffect(cancelStoreGithubBuilds(event.data.deploymentId, runner).pipe(
          Effect.catchIf(newerAttempt, () => Effect.succeed(0)))));
        await step.run("forget-run", () => runEffect(forgetStoreDeploymentRun(runId)));
        return failed;
      }
      const storage = await walkPlacement(event.data, step, runEffect);
      if (storage.kind === "failed") {
        const failed = await step.run("storage-failed", () => runEffect(failStoreDeploymentUnrun(event.data.organizationId, event.data.environmentId, event.data.deploymentId, runId, storage.message, storage.restore)));
        await step.run("mail-failure", () => runEffect(mailGithubDeployTrouble(event.data)));
        return failed;
      }
      if (storage.kind === "done") {
        await step.run("refresh-mirrors", () => runEffect(refreshMirrors(event.data.organizationId, event.data.environmentId)));
      }
      const { organizationId, environmentId, deploymentId } = event.data;
      const lanes = await step.run("place-volumes", () => runEffect(placeDeployVolumes(organizationId, environmentId, deploymentId, runId)));
      for (const lane of lanes) await step.run(`fence-${lane.run.volumeId}`, () => runEffect(fenceDeployVolume(lane, runId)));
      const ran = await step.run("run-deployment", () => runEffect(runStoreDeployment(event.data, storeDeploymentRunner(runId), event.ts)));
      if (lanes.some((lane) => lane.kind === "follow" && lane.ended)) {
        await step.run("release-followed", () => runEffect(releaseFollowed(environmentId, runId, lanes)));
      }
      await step.run("finish-volumes", () => runEffect(finishDeployVolumes(runId, "done")));
      const applied = "ran" in ran && ran.ran.status === "applied";
      // A failed Deployment can still have advanced Applied State, so the checks follow it either way. Only an applied
      // one is probed: a failed one may have stopped partway, and the schedule still checks what it reached.
      await step.run("sync-uptime", () =>
        runEffect(syncUptimeChecks(event.data.organizationId, event.data.environmentId, applied ? event.data.deploymentId : undefined)));
      await step.run("forget-run", () => runEffect(forgetStoreDeploymentRun(runId)));
      if ("ran" in ran && ran.ran.ingress?.length) {
        await step.sendEvent("sync-cluster-domain", clusterDomainSyncRequested.create({ organizationId: event.data.organizationId }));
      }
      // A closed Branch whose removal applied goes now, without its closer coming back for it.
      if (applied && ran.ran.remove) {
        await followUp(step, await step.run("sweep", () => runEffect(sweepStore(event.data.organizationId, new Date()))), runEffect);
      }
      // An applied Deployment can drop a Volume or an Environment, leaving its mirrors orphaned. A removed Environment
      // holds its Volumes' names until the sweep deletes it, so the scan comes after.
      if (applied) {
        await step.run("delete-orphan-mirrors", () => runEffect(cleanOrphans(event.data.organizationId)));
      }
      if ("ran" in ran && deploymentTrouble(ran.ran) !== null) {
        await step.run("mail-failure", () => runEffect(mailGithubDeployTrouble(event.data)));
      }
      return ran;
    },
  );

/**
 * Every minute: a queued Deployment whose hand-off to the worker was lost goes to it again, unkeyed like "Deploy now",
 * since its admission's own event is deduplicated. A duplicate run finds it claimed and runs nothing. A worker run that
 * ended with its stop lost is stopped again, and each GitHub run the Store still owes a cancel is cancelled.
 */
export const createRedispatchStoreDeployments = (inngest: PloyzInngest, runStatus = inngestRunStatus) =>
  inngest.createFunction(
    { id: "redispatch-store-deployments", retries: 1, triggers: [{ cron: "* * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => {
      const unclaimed = await step.run("unclaimed", () => runInngestEffect(unclaimedStoreDeployments(new Date())));
      if (unclaimed.length > 0) await step.sendEvent("dispatch", unclaimed.map(createConfigDeploymentStartedEvent));
      const stale = await step.run("stale-runs", () => runInngestEffect(staleStoreDeploymentRuns(runStatus, new Date())));
      if (stale.length > 0) await step.sendEvent("stop-stale", stale.map((run) => configDeploymentStopRequested.create(run)));
      const owed = await step.run("owed-cancels", () => runInngestEffect(cancelOwedGithubRuns()));
      return { redispatched: unclaimed.map((deployment) => deployment.deploymentId), stopped: stale.map((run) => run.runId), owed };
    },
  );

/** Someone cancelled a run of the worker in Inngest: it stops like a failed one. */
export const createCancelStoreDeployment = (inngest: PloyzInngest, runEffect: EffectRunner<StoreServices> = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "cancel-store-deployment",
      retries: 3,
      triggers: [{ event: inngestFunctionCancelledEventType }],
      concurrency: [{ key: "event.data.run_id", limit: 1 }],
    },
    async ({ event, step }) => {
      // Only the worker's runs are recorded, so any other function's cancellation finds nothing to stop.
      const cancelled = decodeInngestEnvelope(inngestFunctionCancelledEnvelopeSchema)(event);
      await step.run("finish-volumes", () => runEffect(finishDeployVolumes(cancelled.data.run_id, "cancelled")));
      const run = await step.run("find-run", () => runEffect(storeDeploymentRunOf(cancelled.data.run_id)));
      if (run === null) return { skipped: true };
      return stopRun(step, runEffect, { ...run, runId: cancelled.data.run_id, round: 0 });
    },
  );

type Step = Parameters<Parameters<PloyzInngest["createFunction"]>[1]>[0]["step"];

/**
 * Start a Deployment's GitHub builds, in one step when the Store records each. GitHub may have taken a request it
 * never answered, so a build it was asked for is never asked for again: the step records each build before asking for
 * the next, and a record the Store refused gets a step of its own, retried from the first step's memo. The builds after
 * it start in the next round. On the function's `last` try, a run that step still can't record is cancelled before the
 * step fails, as nothing else would stop it.
 */
export async function startGithubBuilds(
  step: Pick<Step, "run">, runEffect: EffectRunner<StoreServices>, data: ConfigDeploymentAdmittedEventData, github: ReadonlyArray<StoreGithubTarget>,
  attempt: string, last = false,
): Promise<ReadonlyArray<StoreGithubStart>> {
  const started: StoreGithubStart[] = [];
  let rest = github;
  for (let round = 0; rest.length > 0; round += 1) {
    const targets = rest;
    const starts = await step.run(`github-start-round-${round}`, () => runEffect(startStoreGithubBuilds(data.organizationId, data.deploymentId, targets, attempt)));
    started.push(...starts.started);
    const { unrecorded } = starts;
    if (unrecorded !== null) started.push(await step.run(`github-record-round-${round}`, () => runEffect(recordStoreGithubBuild(unrecorded, last))));
    rest = starts.rest;
  }
  return started;
}

/**
 * One build's go on GitHub, before the runner claims the Deployment: start it, then wait for its run while the runner
 * checks in and pushes. Each wait first reads whether a final report already ended it; each timeout also asks GitHub,
 * which catches a completion that landed between waits. With a Builder after GitHub, the first wait is the start limit.
 * Whatever GitHub doesn't build, the runner builds on the servers when the walk has them next, or fails with why.
 */
async function walkGithub(
  organizationId: string, target: StoreGithubTarget, started: StoreGithubStart | undefined, attempt: string, step: Step,
  runEffect: EffectRunner<StoreServices>,
): Promise<string | null> {
  const key = target.service;
  if (started?.kind !== "dispatched") return null;
  for (let check = 0; ; check += 1) {
    if (!(await step.run(`github-open-${key}-${check}`, () => runEffect(storeGithubBuildOpen(target))))) return null;
    const startLimit = check === 0 && target.hasNext;
    const ended = await step.waitForEvent(`github-wait-${key}-${check}`, {
      event: githubBuildRunCompleted.name, if: `async.data.runId == ${started.runId}`,
      timeout: startLimit ? `${START_WITHIN_MINUTES}m` : GITHUB_CHECK_INTERVAL,
    });
    const seen = { ended: ended !== null, startLimit };
    const runId = started.runId;
    const found = await step.run(`github-check-${key}-${check}`, () => runEffect(checkStoreGithubBuild(organizationId, target, seen, attempt, runId)));
    if (found === "done") return null;
    if (found !== "waiting") return found.failed;
  }
}

const PLACEMENT_PULLS = 144;
const PLACEMENT_PULL_WAIT = "10m";

/**
 * The finished event names how the run ended, so the Deploy closes the row itself rather than wait for the closer; a wait
 * that times out looks at the row again, since the event can be lost.
 */
async function walkPlacement(data: ConfigDeploymentAdmittedEventData, step: Step, runEffect: EffectRunner<StoreServices>): Promise<PlacementAdvance> {
  let waited: PlacementWait | null = null;
  for (let pull = 0; pull < PLACEMENT_PULLS; pull += 1) {
    const last = waited;
    const advanced = await step.run(`placement-${pull}`, () => runEffect(advancePlacement(data.organizationId, data.deploymentId, last)));
    if (advanced.kind !== "wait") return advanced;
    waited = advanced.wait;
    const finished = await step.waitForEvent(`placement-pull-${pull}`, {
      event: inngestFunctionFinishedEventType, if: `async.data.event.data.runId == '${advanced.wait.runId}'`, timeout: PLACEMENT_PULL_WAIT,
    });
    if (finished !== null) {
      const { runId, end } = finishedVolumeRun(finished);
      await step.run(`placement-close-${pull}`, () => runEffect(closeVolumeRun(runId, end)));
    }
  }
  return { kind: "failed", message: "Its Volumes were still copying after a day. Deploy again once they finish." };
}

/**
 * The Organization's first Server joined: each Environment with published state deploys to it. Each admission's id is
 * minted in its own step, so a retry after an unrecorded commit replays that admission rather than admitting again.
 */
export const createDeployToFirstServer = (inngest: PloyzInngest, runEffect: EffectRunner<StoreServices> = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "deploy-to-first-server", retries: 3, triggers: [{ event: configFirstServerJoined.eventType }],
      concurrency: [{ key: "event.data.organizationId", limit: 1 }],
    },
    async ({ event, step }) => {
      const { organizationId } = event.data;
      const environments = await step.run("published", () => runEffect(publishedEnvironments(organizationId)));
      const deployed = [];
      for (const environment of environments) {
        const name = `${environment.project}/${environment.environment}`;
        const id = await step.run(`admission-${name}`, () => crypto.randomUUID());
        deployed.push({ environment: name, ...await step.run(`deploy-${name}`, () => runEffect(deployToFirstServer(organizationId, environment, id))) });
      }
      return deployed;
    },
  );
