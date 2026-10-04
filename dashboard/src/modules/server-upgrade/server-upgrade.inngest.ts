import { Effect, Option, Schema } from "effect";
import { attemptLifecycle } from "#/modules/inngest/attempt-lifecycle";
import type { PloyzStepTools } from "#/modules/inngest/client";
import { createServerUpgradeRequestedEvent, serverUpgradeRequestedEventType } from "#/modules/inngest/events";
import { machineIdStringSchema } from "#/modules/machines/enrollment";
import { type FinalOutcome, type ReleaseChannel, UPGRADE_TRIGGERS } from "#/modules/server-upgrade/server-upgrade";
import {
  closeRunUpgradeAttempts,
  closeStaleUpgradeAttempts,
  finalOutcome,
  finishUpgradeAttempt,
  listAutomaticUpgradeOrganizationIds,
  listServersBehind,
  mintAttemptId,
  observeUpgradeableServer,
  organizationReleaseChannel,
  pollUpgradeOnServer,
  recordUpgradeAttempt,
  requestUpgradeOnServer,
} from "#/modules/server-upgrade/server-upgrade.server";
import type { runInngestEffect } from "#/server/run.server";

type StepTools = Pick<PloyzStepTools, "run" | "sleep">;
type EffectRunner = typeof runInngestEffect;

const POLL_INTERVAL_MS = 15_000;

const ServerUpgradeRequestedData = Schema.Struct({
  organizationId: Schema.String.check(Schema.isNonEmpty()),
  machineId: Schema.NullOr(machineIdStringSchema),
  trigger: Schema.Literals(UPGRADE_TRIGGERS),
  userId: Schema.NullOr(Schema.String),
});
type Request = Omit<typeof ServerUpgradeRequestedData.Type, "machineId"> & { readonly channel: ReleaseChannel };
/** One Server's part of a Rollout: skipped with nothing recorded, or attempted to an outcome. */
type ServerResult =
  | { readonly kind: "skipped"; readonly reason: "not-online" | "busy" }
  | { readonly kind: "attempted"; readonly attemptId: string; readonly outcome: FinalOutcome };

/**
 * One Rollout run along the Organization's Release Channel: the named Server, or every Server behind (online, idle,
 * older than the newest release on the channel for its line) in name order, one at a time; an automatic one skips a
 * Halted release. It stops at the first outcome that isn't `succeeded`; a Server that went
 * offline or refuses as Busy is skipped and records nothing.
 */
export async function executeRollOutServerUpgrade(
  { event, step, runId }: { event: { data: unknown }; step: StepTools; runId: string },
  runEffect: EffectRunner,
) {
  const request = await step.run("normalize-request", () => {
    const decoded = Schema.decodeUnknownOption(ServerUpgradeRequestedData)(event.data, { onExcessProperty: "preserve" });
    return Option.isSome(decoded) ? decoded.value : null;
  });
  if (request === null) return { skipped: "invalid" as const };
  const { machineId, ...rest } = request;
  const channel = await step.run("read-channel", () => runEffect(organizationReleaseChannel(request.organizationId)));
  const machineIds = machineId === null
    ? await step.run("pick-servers", () => runEffect(listServersBehind(request.organizationId, request.trigger, channel)))
    : [machineId];

  const results = [];
  for (const id of machineIds) {
    const result = await upgradeServer({ request: { ...rest, channel }, machineId: id, step, runId }, runEffect);
    results.push({ machineId: id, ...result });
    if (result.kind === "attempted" && result.outcome !== "succeeded") break;
  }
  return { results };
}

/**
 * Upgrade one Server: observe it online and idle → record the attempt → request the Upgrade along the channel → poll
 * until the outcome is terminal → record it. Twenty minutes from the attempt's `startedAt` without one records
 * `unknown` with the last stage seen.
 */
async function upgradeServer(
  { request, machineId, step, runId }: { request: Request; machineId: string; step: StepTools; runId: string },
  runEffect: EffectRunner,
): Promise<ServerResult> {
  const { organizationId, channel, ...event } = request;
  const observed = await step.run(`observe-server-${machineId}`, async () => {
    const fromVersion = await runEffect(observeUpgradeableServer(organizationId, machineId));
    // Minted here so every retry of a later step reuses it.
    return fromVersion === null ? null : { fromVersion, attemptId: mintAttemptId() };
  });
  if (observed === null) return { kind: "skipped", reason: "not-online" };
  const { fromVersion, attemptId } = observed;
  await step.run(`record-attempt-${machineId}`, () => runEffect(recordUpgradeAttempt({
    request: { organizationId, ...event, machineId },
    attemptId,
    channel,
    fromVersion,
    inngestRunId: runId,
  })));
  const attempt = await step.run(`request-upgrade-${machineId}`, () =>
    runEffect(requestUpgradeOnServer({ organizationId, machineId, attemptId, channel })));
  if (attempt === null) return { kind: "skipped", reason: "busy" };

  let outcome = finalOutcome(attempt);
  poll: for (let poll = 0; outcome === null; poll += 1) {
    await step.sleep(`wait-${machineId}-${poll}`, POLL_INTERVAL_MS);
    const polled = await step.run(`inspect-upgrade-${machineId}-${poll}`, () =>
      runEffect(pollUpgradeOnServer({ organizationId, machineId, attemptId })));
    switch (polled.kind) {
      case "expired":
        break poll;
      case "unreadable":
        continue;
      case "read":
        outcome = finalOutcome(polled.attempt);
    }
  }
  const recorded = outcome ?? { outcome: "unknown" as const, stage: null, error: null };
  await step.run(`record-outcome-${machineId}`, () => runEffect(finishUpgradeAttempt({ organizationId, attemptId, ...recorded })));
  return { kind: "attempted", attemptId, outcome: recorded.outcome };
}

const lifecycle = attemptLifecycle({
  run: {
    id: "roll-out-server-upgrade",
    triggers: [{ event: serverUpgradeRequestedEventType }],
    concurrency: [{ key: "event.data.organizationId", limit: 1 }],
    handler: executeRollOutServerUpgrade,
  },
  cancelId: "cancel-server-upgrade",
  closeRun: closeRunUpgradeAttempts,
  sweep: {
    id: "schedule-server-upgrades",
    closeStale: () => Effect.map(closeStaleUpgradeAttempts(), (closed) => ({ closed })),
    afterSweep: async (step, runEffect) => {
      const organizationIds = await step.run("list-automatic-organizations", () => runEffect(listAutomaticUpgradeOrganizationIds()));
      if (organizationIds.length > 0) {
        await step.sendEvent("request-automatic-rollouts", organizationIds.map((organizationId) =>
          createServerUpgradeRequestedEvent({ organizationId, machineId: null, trigger: "automatic", userId: null })));
      }
      return { organizationCount: organizationIds.length };
    },
  },
});

export const createRollOutServerUpgrade = lifecycle.createRun;
export const createCancelServerUpgrade = lifecycle.createCancel;
export const createScheduleServerUpgrades = lifecycle.createSweep;
export const createServerUpgradeFunctions = lifecycle.createFunctions;
