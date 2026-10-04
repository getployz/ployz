import { Option, Schema } from "effect";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import {
  inngestFunctionCancelledEnvelopeSchema,
  inngestFunctionCancelledEventType,
  serverUpgradeRequestedEventType,
} from "#/modules/inngest/events";
import { UPGRADE_OBSERVATION_LIMIT_MS, UPGRADE_TRIGGERS } from "#/modules/server-upgrade/server-upgrade";
import {
  closeRunUpgradeAttempts,
  finalOutcome,
  finishUpgradeAttempt,
  inspectUpgradeOnServer,
  mintAttemptId,
  observeUpgradeableServer,
  recordUpgradeAttempt,
  requestUpgradeOnServer,
} from "#/modules/server-upgrade/server-upgrade.server";
import { runInngestEffect } from "#/server/run.server";

export const ROLL_OUT_SERVER_UPGRADE_FUNCTION_ID = "roll-out-server-upgrade";

type StepTools = Pick<PloyzStepTools, "run" | "sleep">;
type EffectRunner = typeof runInngestEffect;

const POLL_INTERVAL_MS = 15_000;
// ponytail: counts polls rather than reading a clock; each inspect adds its own time, so the limit is a floor.
const POLLS = UPGRADE_OBSERVATION_LIMIT_MS / POLL_INTERVAL_MS;

const MachineId = Schema.String.check(Schema.isPattern(/^[0-9a-f]{32}$/u));
const ServerUpgradeRequestedData = Schema.Struct({
  organizationId: Schema.String.check(Schema.isNonEmpty()),
  machineId: MachineId,
  trigger: Schema.Literals(UPGRADE_TRIGGERS),
  userId: Schema.NullOr(Schema.String),
});

const decodeFailedRun = Schema.decodeUnknownOption(Schema.Struct({ data: Schema.Struct({ run_id: Schema.String }) }));

/**
 * One rollout run: observe the clicked Server online and idle → record the attempt → request the Upgrade along
 * `stable` → poll until the outcome is terminal → record it. Twenty minutes without one records `unknown` with the
 * last stage seen. A Busy refusal records nothing.
 */
export async function executeRollOutServerUpgrade(
  { event, step, runId }: { event: { data: unknown }; step: StepTools; runId: string },
  runEffect: EffectRunner,
) {
  const request = await step.run("normalize-request", () => {
    const decoded = Schema.decodeUnknownOption(ServerUpgradeRequestedData)(event.data, { onExcessProperty: "preserve" });
    // Minted here so every retry of a later step reuses it.
    return Option.isSome(decoded) ? { ...decoded.value, attemptId: mintAttemptId() } : null;
  });
  if (request === null) return { skipped: "invalid" as const };
  const { organizationId, machineId, attemptId } = request;
  const channel = "stable" as const;

  const fromVersion = await step.run("observe-server", () => runEffect(observeUpgradeableServer(organizationId, machineId)));
  if (fromVersion === null) return { skipped: "not-online" as const };
  await step.run("record-attempt", () => runEffect(recordUpgradeAttempt({
    request: { organizationId, machineId, trigger: request.trigger, userId: request.userId },
    attemptId,
    channel,
    fromVersion,
    inngestRunId: runId,
  })));
  let attempt = await step.run("request-upgrade", () => runEffect(requestUpgradeOnServer({ organizationId, machineId, attemptId, channel })));
  if (attempt === null) return { skipped: "busy" as const };

  let outcome = finalOutcome(attempt);
  for (let poll = 0; outcome === null && poll < POLLS; poll += 1) {
    await step.sleep(`wait-${poll}`, POLL_INTERVAL_MS);
    attempt = (await step.run(`inspect-upgrade-${poll}`, () =>
      runEffect(inspectUpgradeOnServer({ organizationId, machineId, attemptId })))) ?? attempt;
    outcome = finalOutcome(attempt);
  }
  const recorded = outcome ?? { outcome: "unknown" as const, stage: null, error: null };
  await step.run("record-outcome", () => runEffect(finishUpgradeAttempt({ organizationId, attemptId, ...recorded })));
  return { attemptId, outcome: recorded.outcome };
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
