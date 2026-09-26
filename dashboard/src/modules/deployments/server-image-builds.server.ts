import "@tanstack/react-start/server-only";
import type { BuildReceipt, MachineId } from "@ployz/sdk";
import { Effect, type Types } from "effect";
import type { SkipReason } from "./image-build";
import { errorEvidenceFrom } from "#/lib/error-evidence";
import { PloyzPreparationError } from "#/modules/runtime/ployz.server";
import { Database, ReportingDatabase } from "#/server/database.server";
import type { BuildCandidate } from "./build-order";
import { persistBuildLog } from "./deployment-events.server";
import { deploymentReporting } from "./deployment-reporting.server";
import {
  imageBuildWanted, loadBuildReceipts, recordServerChoice, settleImageBuild, skipUnstarted, START_WITHIN_MINUTES,
  type ImageBuildTarget,
} from "./image-builds.server";
import { ployzStep, preparationProgressCollector, type BuildStepWrite } from "./preparation-progress";
import { loadDeploymentContext } from "./runtime-hydration.repository.server";
import { connectedRuntime, oneServiceDeployment, watchDeploymentCancellation } from "./runtime-session.server";
import { acquireDeploymentSources } from "./runtime-sources.server";

/** A build that reused an image still leaves this one Build Step as its evidence. */
export const reusedImageStep = (): BuildStepWrite => ({ ...ployzStep("stage:Reused", "Reused image"), cached: true });

/** How one go on the Cluster ended, before the Image Build records it. */
type ClusterBuild =
  | { kind: "queued" }
  | { kind: "built"; receipt: BuildReceipt }
  | { kind: "failed"; message: string; stage: string | null }
  | { kind: "cancelled"; message: string; stage: string | null };

/**
 * Your servers as a Builder: one Image Build on the Organization Cluster, whose Engine chooses the
 * Server (a Preferred Server first). With `startWithinMs`, a build no Server admitted in time is
 * withdrawn before any source is uploaded, and the servers are skipped. Otherwise it settles the
 * row on every exit. It stops when its attempt ends or is cancelled.
 */
export const buildOnServers = Effect.fn("Deployments.buildOnServers")(function* (
  build: ImageBuildTarget, candidate: Pick<BuildCandidate, "machineId" | "attempt">, startWithinMs?: number,
) {
  const cancellation = new AbortController();
  const collector = preparationProgressCollector();
  const reporting = deploymentReporting();
  let machineId: MachineId | null = null;
  let machineName: string | undefined;
  let logged = false;
  const log = (writes: { steps: BuildStepWrite[]; output: { build: number; step: string; stderr: boolean; text: string }[] }) => {
    logged ||= writes.steps.length > 0;
    return reporting.write(persistBuildLog(build.deploymentId, writes, { image: build.image, attempt: candidate.attempt }));
  };
  const outcome: ClusterBuild = yield* Effect.gen(function* () {
    const context = yield* loadDeploymentContext(build.deploymentId);
    const snapshot = context?.snapshots.find((candidate) => candidate.serviceId === build.serviceId);
    if (!context || !snapshot) return { kind: "failed", message: "The Service is no longer part of this deployment.", stage: null } satisfies ClusterBuild;
    const { sources, source_commits } = yield* acquireDeploymentSources({ ...context, snapshots: [snapshot] }, () => Effect.void);
    const sdk = yield* connectedRuntime(context.organization.id);
    const deployment = yield* oneServiceDeployment(context, build.serviceId);
    const hint = (yield* loadBuildReceipts({ environmentId: context.environment.id }))[build.image];
    const progressContext = yield* Effect.context<Database | ReportingDatabase>();
    const result = yield* sdk.build({
      deployment, sources, source_commits, build_receipts: hint ? { [build.image]: hint } : {},
      build_index: build.buildIndex, preferred_machine: candidate.machineId,
    }, async (event) => {
      if (event !== "Transfer" && "Selected" in event) {
        const { machine, reason } = event.Selected;
        machineId = machine.id;
        machineName = machine.name;
        await Effect.runPromiseWith(progressContext)(recordServerChoice(build.id, machine.id, { machineName: machine.name, reason }));
      }
      const writes = collector.event(event);
      await Effect.runPromiseWith(progressContext)(log(writes));
    }, { signal: cancellation.signal, startWithinMs });
    return result.kind === "queued" ? { kind: "queued" as const } : { kind: "built" as const, receipt: result.receipt };
  }).pipe(
    Effect.scoped,
    Effect.raceFirst(watchDeploymentCancellation(imageBuildWanted(build.id), cancellation)),
    Effect.catch((error) => {
      const failure = errorEvidenceFrom(error);
      const stage = error instanceof PloyzPreparationError ? error.stage ?? null : null;
      const message = failure.message || "Build failed.";
      return Effect.succeed<ClusterBuild>(cancellation.signal.aborted || failure.failureCode === "sdk_preparation_cancelled"
        ? { kind: "cancelled", message, stage }
        : { kind: "failed", message, stage });
    }),
  );
  // The build log closes once, whichever way the go ended.
  const failed = outcome.kind === "failed" || outcome.kind === "cancelled" ? outcome : null;
  const steps = collector.finish(failed?.message ?? null, failed?.stage ?? null);
  if (outcome.kind === "built" && !logged && !steps.length) steps.push(reusedImageStep());
  yield* log({ steps, output: [] });
  switch (outcome.kind) {
    case "queued": {
      // The Server's name leaves the row with the skip; the skip keeps it for the log's heading.
      const reason: Types.Mutable<SkipReason> = { builder: "servers", kind: "not_started", minutes: START_WITHIN_MINUTES };
      if (machineName !== undefined) reason.machineName = machineName;
      return yield* skipUnstarted(build, reason);
    }
    case "built": return yield* settleImageBuild(build, { status: "built", receipt: outcome.receipt });
    case "failed": return yield* settleImageBuild(build, { status: "failed", message: outcome.message, machineId });
    case "cancelled": return yield* settleImageBuild(build, { status: "cancelled" });
  }
});
