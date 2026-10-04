import { Option, Schema } from "effect";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import {
  createServerUpgradeRequestedEvent,
  inngestFunctionCancelledEnvelopeSchema,
  inngestFunctionCancelledEventType,
  serverUpgradeRequestedEventType,
} from "#/modules/inngest/events";
import { UPGRADE_OBSERVATION_LIMIT_MS, UPGRADE_TRIGGERS } from "#/modules/server-upgrade/server-upgrade";
import {
  closeRunUpgradeAttempts,
  closeStaleUpgradeAttempts,
  finalOutcome,
  finishUpgradeAttempt,
  inspectUpgradeOnServer,
  listAutomaticUpgradeOrganizationIds,
  listServersBehind,
  mintAttemptId,
  observeUpgradeableServer,
  recordUpgradeAttempt,
  requestUpgradeOnServer,
} from "#/modules/server-upgrade/server-upgrade.server";
import { runInngestEffect } from "#/server/run.server";

export const ROLL_OUT_SERVER_UPGRADE_FUNCTION_ID = "roll-out-server-upgrade";

type StepTools = Pick<PloyzStepTools, "run" | "sleep">;
type ScheduleStepTools = Pick<PloyzStepTools, "run" | "sendEvent">;
type EffectRunner = typeof runInngestEffect;

const POLL_INTERVAL_MS = 15_000;
// ponytail: counts polls rather than reading a clock; each inspect adds its own time, so the limit is a floor.
const POLLS = UPGRADE_OBSERVATION_LIMIT_MS / POLL_INTERVAL_MS;

const MachineId = Schema.String.check(Schema.isPattern(/^[0-9a-f]{32}$/u));
const ServerUpgradeRequestedData = Schema.Struct({
  organizationId: Schema.String.check(Schema.isNonEmpty()),
  machineId: Schema.NullOr(MachineId),
  trigger: Schema.Literals(UPGRADE_TRIGGERS),
  userId: Schema.NullOr(Schema.String),
});
type Request = Omit<typeof ServerUpgradeRequestedData.Type, "machineId">;

const decodeFailedRun = Schema.decodeUnknownOption(Schema.Struct({ data: Schema.Struct({ run_id: Schema.String }) }));

/**
 * One rollout run: the named Server, or every Server behind (online, idle, older than the newest release on its line)
 * in name order, one at a time; an automatic one skips a release the Organization is halted on. It stops at the first outcome that isn't `succeeded`; a Server that went offline or
 * refuses as Busy is skipped and records nothing.
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
  const machineIds = machineId === null
    ? await step.run("pick-servers", () => runEffect(listServersBehind(request.organizationId, request.trigger)))
    : [machineId];

  const results = [];
  for (const id of machineIds) {
    const result = await upgradeServer({ request: rest, machineId: id, step, runId }, runEffect);
    results.push({ machineId: id, ...result });
    if ("outcome" in result && result.outcome !== "succeeded") break;
  }
  return { results };
}

/**
 * Upgrade one Server: observe it online and idle → record the attempt → request the Upgrade along `stable` → poll
 * until the outcome is terminal → record it. Twenty minutes without one records `unknown` with the last stage seen.
 */
async function upgradeServer(
  { request, machineId, step, runId }: { request: Request; machineId: string; step: StepTools; runId: string },
  runEffect: EffectRunner,
) {
  const { organizationId } = request;
  const channel = "stable" as const;
  const observed = await step.run(`observe-server-${machineId}`, async () => {
    const fromVersion = await runEffect(observeUpgradeableServer(organizationId, machineId));
    // Minted here so every retry of a later step reuses it.
    return fromVersion === null ? null : { fromVersion, attemptId: mintAttemptId() };
  });
  if (observed === null) return { skipped: "not-online" as const };
  const { fromVersion, attemptId } = observed;
  await step.run(`record-attempt-${machineId}`, () => runEffect(recordUpgradeAttempt({
    request: { ...request, machineId },
    attemptId,
    channel,
    fromVersion,
    inngestRunId: runId,
  })));
  let attempt = await step.run(`request-upgrade-${machineId}`, () =>
    runEffect(requestUpgradeOnServer({ organizationId, machineId, attemptId, channel })));
  if (attempt === null) return { skipped: "busy" as const };

  let outcome = finalOutcome(attempt);
  for (let poll = 0; outcome === null && poll < POLLS; poll += 1) {
    await step.sleep(`wait-${machineId}-${poll}`, POLL_INTERVAL_MS);
    attempt = (await step.run(`inspect-upgrade-${machineId}-${poll}`, () =>
      runEffect(inspectUpgradeOnServer({ organizationId, machineId, attemptId })))) ?? attempt;
    outcome = finalOutcome(attempt);
  }
  const recorded = outcome ?? { outcome: "unknown" as const, stage: null, error: null };
  await step.run(`record-outcome-${machineId}`, () => runEffect(finishUpgradeAttempt({ organizationId, attemptId, ...recorded })));
  return { attemptId, outcome: recorded.outcome };
}

/**
 * The hourly run: close attempts a dead rollout run left `running` → request one automatic rollout per Organization
 * with automatic upgrades on, as the hourly Cluster Domain sync fans out.
 */
export async function executeScheduleServerUpgrades({ step }: { step: ScheduleStepTools }, runEffect: EffectRunner) {
  const closed = await step.run("close-stale-attempts", () => runEffect(closeStaleUpgradeAttempts()));
  const organizationIds = await step.run("list-automatic-organizations", () => runEffect(listAutomaticUpgradeOrganizationIds()));
  if (organizationIds.length > 0) {
    await step.sendEvent("request-automatic-rollouts", organizationIds.map((organizationId) =>
      createServerUpgradeRequestedEvent({ organizationId, machineId: null, trigger: "automatic", userId: null })));
  }
  return { closed, organizationCount: organizationIds.length };
}

/** A cancelled rollout run must not leave its attempt `running`. */
export async function executeCancelServerUpgrade({ event, step }: { event: unknown; step: Pick<StepTools, "run"> }, runEffect: EffectRunner) {
  const decoded = await step.run("decode-cancellation", () => decodeInngestEnvelope(inngestFunctionCancelledEnvelopeSchema)(event));
  if (decoded.data.function_id !== ROLL_OUT_SERVER_UPGRADE_FUNCTION_ID) return { skipped: true };
  const runId = decoded.data.run_id;
  return { closed: await step.run("close-attempts", () => runEffect(closeRunUpgradeAttempts(runId))) };
}

export const createRollOutServerUpgrade = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: ROLL_OUT_SERVER_UPGRADE_FUNCTION_ID,
      retries: 3,
      triggers: [{ event: serverUpgradeRequestedEventType }],
      // One rollout per Organization at a time.
      concurrency: [{ key: "event.data.organizationId", limit: 1 }],
      onFailure: async ({ event }) => {
        // `inngest/function.failed` names the failed run.
        const failed = decodeFailedRun(event);
        if (Option.isSome(failed)) await runEffect(closeRunUpgradeAttempts(failed.value.data.run_id));
      },
    },
    async ({ event, step, runId }) => executeRollOutServerUpgrade({ event, step, runId }, runEffect),
  );

export const createCancelServerUpgrade = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "cancel-server-upgrade",
      retries: 3,
      triggers: [{ event: inngestFunctionCancelledEventType }],
      concurrency: [{ key: "event.data.run_id", limit: 1 }],
    },
    async ({ event, step }) => executeCancelServerUpgrade({ event, step }, runEffect),
  );

export const createScheduleServerUpgrades = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "schedule-server-upgrades",
      retries: 3,
      triggers: [{ cron: "TZ=UTC 0 * * * *" }],
      concurrency: [{ limit: 1 }],
    },
    async ({ step }) => executeScheduleServerUpgrades({ step }, runEffect),
  );
