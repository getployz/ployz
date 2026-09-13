import { Option, Effect, Result as EffectResult, Schema } from "effect";
import { NonRetriableError } from "inngest";
import {
  drainGithubCheckSuiteTransitionOutbox,
  drainGithubEnvironmentTriggerOutbox,
  publishPendingGithubCheckSuiteTransition,
  type GithubIngestionStepTools,
} from "#/modules/github/inngest-ingestion/outbox";
import {
  compareInstallationRepositoryCommits,
  fetchInstallationCheckSuite,
  GithubApi,
  isGithubObservationNotFound,
  resolveInstallationBranchHead,
  resolveInstallationRepository,
  type GithubResolvedRepository,
} from "#/modules/github/github-observation.api";
import {
  applyGithubBranchEvaluation,
  applyGithubCheckSuiteTestimony,
  failGithubDelivery,
  GithubIngestionRepositoryError,
  listGithubServiceCandidates,
  loadGithubBranchCursor,
  recordAndClaimGithubDelivery,
  type GithubBranchEvaluationResult,
  type GithubCheckSuiteTestimonyResult,
} from "#/modules/github/github-ingestion.repository";
import {
  githubCheckSuiteReceivedEventDataSchema,
  githubPushReceivedEventDataSchema,
  type GithubPushReceivedEventData,
} from "#/modules/github/github-ingestion.contracts";
import {
  githubCheckSuiteReceivedEvent,
  githubCheckSuiteReceivedEventType,
  githubPushReceivedEvent,
  githubPushReceivedEventType,
  inngestEventEnvelopeFields,
  inngestFunctionFailedEnvelopeSchema,
} from "#/modules/inngest/events";
import type { PloyzInngest, InngestClient } from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import {
  PROCESS_GITHUB_CHECK_SUITE_RECEIVED_FUNCTION_ID,
  PROCESS_GITHUB_PUSH_RECEIVED_FUNCTION_ID,
} from "#/modules/inngest/row-backed-workflow-ids";
import {
  type GithubBranchComparison,
  planGithubBranchEvaluation,
} from "#/modules/github/github-branch-evaluation";
import { runInngestEffect } from "#/server/run.server";
import type { AppConfig } from "#/server/config.server";
import type { Database } from "#/server/database.server";

export type GithubIngestionEffectRunner = <A, E extends Error>(
  effect: Effect.Effect<
    A,
    E,
    AppConfig | Database | InngestClient | GithubApi
  >,
) => Promise<A>;

const GithubPushReceivedEnvelope = Schema.Struct({
  ...inngestEventEnvelopeFields,
  name: Schema.Literal(githubPushReceivedEvent),
  data: githubPushReceivedEventDataSchema,
});
const GithubCheckSuiteReceivedEnvelope = Schema.Struct({
  ...inngestEventEnvelopeFields,
  name: Schema.Literal(githubCheckSuiteReceivedEvent),
  data: githubCheckSuiteReceivedEventDataSchema,
});
const GithubIngestionFailureEnvelope = inngestFunctionFailedEnvelopeSchema(
  Schema.Union([
    GithubPushReceivedEnvelope,
    GithubCheckSuiteReceivedEnvelope,
  ]),
);

type UntrustedInngestEnvelope = {
  readonly name?: unknown;
  readonly data?: unknown;
};

type BranchApplyOutcome =
  | { kind: "applied"; value: GithubBranchEvaluationResult }
  | { kind: "cursor_conflict" };

type CheckSuiteApplyOutcome =
  | { kind: "applied"; value: GithubCheckSuiteTestimonyResult }
  | { kind: "pending_publication" };

function isCursorConflict(
  cause: unknown,
): cause is GithubIngestionRepositoryError {
  return (
    cause instanceof GithubIngestionRepositoryError &&
    cause.code === "cursor_conflict"
  );
}

function isPendingPublication(
  cause: unknown,
): cause is GithubIngestionRepositoryError {
  return (
    cause instanceof GithubIngestionRepositoryError &&
    cause.code === "pending_publication"
  );
}

export const GITHUB_PUSH_RECEIVED_CONCURRENCY = [
  { key: "event.data.branchKey", limit: 1 },
] as const;
export const GITHUB_CHECK_SUITE_RECEIVED_CONCURRENCY = [
  { key: "event.data.checkSuiteKey", limit: 1 },
] as const;

async function processPushAttempt(input: {
  payload: GithubPushReceivedEventData;
  receipt: { receiptSequence: number };
  repository: GithubResolvedRepository;
  step: GithubIngestionStepTools;
  attempt: number;
  processingRunId: string;
  runEffect: GithubIngestionEffectRunner;
}): Promise<BranchApplyOutcome> {
  const {
    payload,
    receipt,
    repository,
    step,
    attempt,
    processingRunId,
    runEffect,
  } = input;
  const liveBranch = await step.run(`resolve-live-branch-head-${attempt}`, () =>
    runEffect(
      resolveInstallationBranchHead(
        payload.installationId,
        repository,
        payload.ref,
      ),
    ),
  );
  const cursor = await step.run(`load-branch-cursor-${attempt}`, () =>
    runEffect(
      loadGithubBranchCursor({
        installationId: payload.installationId,
        repositoryId: payload.repositoryId,
        ref: payload.ref,
      }),
    ),
  );

  let comparison: GithubBranchComparison = { state: "not_required" };
  if (
    liveBranch.state === "present" &&
    cursor?.state === "active" &&
    cursor.headSha !== liveBranch.headSha &&
    !payload.forced
  ) {
    comparison = await step.run(
      `compare-cursor-to-live-head-${attempt}`,
      () =>
        runEffect(
          compareInstallationRepositoryCommits(
            payload.installationId,
            repository,
            cursor.headSha,
            liveBranch.headSha,
          ).pipe(
            Effect.map(
              (value): GithubBranchComparison => ({
                state: "observed",
                value,
              }),
            ),
            Effect.catchIf(isGithubObservationNotFound, () =>
              Effect.succeed({ state: "not_found" as const }),
            ),
          ),
        ),
    );
  }

  const needsCandidates =
    liveBranch.state === "present" &&
    !(cursor?.state === "active" && cursor.headSha === liveBranch.headSha);
  const candidates = needsCandidates
    ? await step.run(`list-service-candidates-${attempt}`, () =>
        runEffect(
          listGithubServiceCandidates({
            installationId: payload.installationId,
            repositoryId: payload.repositoryId,
            ref: payload.ref,
          }),
        ),
      )
    : [];
  const plan = await step.run(`plan-branch-evaluation-${attempt}`, () => {
    const planned = planGithubBranchEvaluation({
      cursor,
      liveBranch,
      forced: payload.forced,
      comparison,
      candidates,
    });
    if (EffectResult.isFailure(planned)) {
      throw new NonRetriableError(planned.failure.message, {
        cause: planned.failure,
      });
    }
    return planned.success;
  });

  return step.run(`apply-branch-evaluation-${attempt}`, () =>
    runEffect(
      applyGithubBranchEvaluation({
        deliveryId: payload.deliveryId,
        receiptSequence: receipt.receiptSequence,
        installationId: payload.installationId,
        repositoryId: payload.repositoryId,
        ref: payload.ref,
        processingRunId,
        expectedCursor: cursor,
        plan,
      }).pipe(
        Effect.map(
          (value): BranchApplyOutcome => ({ kind: "applied", value }),
        ),
        Effect.catchIf(isCursorConflict, () =>
          Effect.succeed({ kind: "cursor_conflict" as const }),
        ),
      ),
    ),
  );
}

export async function failGithubIngestionDeliveryOnRetryExhausted(
  event: UntrustedInngestEnvelope,
  runEffect: GithubIngestionEffectRunner,
) {
  const decoded = Schema.decodeUnknownOption(GithubIngestionFailureEnvelope)(
    event,
  );
  if (Option.isNone(decoded)) return;
  const processingRunId = decoded.value.data.run_id;
  await runEffect(
    failGithubDelivery({
      processingRunId,
      failureCode: "retry_exhausted",
    }),
  );
}

export async function executeProcessGithubPushReceived(
  input: {
    event: UntrustedInngestEnvelope;
    step: GithubIngestionStepTools;
    runId: string;
  },
  runEffect: GithubIngestionEffectRunner,
) {
  const payload = await input.step.run("decode-push-event", () =>
    decodeInngestEnvelope(GithubPushReceivedEnvelope)(input.event).data,
  );
  const receipt = await input.step.run("record-and-claim-delivery", () =>
    runEffect(
      recordAndClaimGithubDelivery({
        deliveryId: payload.deliveryId,
        eventKind: "push",
        installationId: payload.installationId,
        repositoryId: payload.repositoryId,
        ref: payload.ref,
        branch: payload.deleted
          ? { state: "deleted" }
          : { state: "active", headSha: payload.afterSha },
        processingRunId: input.runId,
      }),
    ),
  );
  if (receipt.disposition === "terminal") {
    await drainGithubEnvironmentTriggerOutbox(
      input.step,
      "push-replay",
      runEffect,
    );
    return receipt;
  }
  if (receipt.disposition === "owned_elsewhere") return receipt;
  const repository = await input.step.run("resolve-repository", () =>
    runEffect(
      resolveInstallationRepository(
        payload.installationId,
        payload.repositoryId,
      ),
    ),
  );
  for (let attempt = 0; attempt < 3; attempt += 1) {
    const applied = await processPushAttempt({
      payload,
      receipt,
      repository,
      step: input.step,
      attempt,
      processingRunId: input.runId,
      runEffect,
    });
    if (applied.kind === "applied") {
      await drainGithubEnvironmentTriggerOutbox(input.step, "push", runEffect);
      return applied.value;
    }
  }
  throw new Error("GitHub branch cursor kept changing during evaluation.");
}

export async function executeProcessGithubCheckSuiteReceived(
  input: {
    event: UntrustedInngestEnvelope;
    step: GithubIngestionStepTools;
    runId: string;
  },
  runEffect: GithubIngestionEffectRunner,
) {
  const payload = await input.step.run("decode-check-suite-event", () =>
    decodeInngestEnvelope(GithubCheckSuiteReceivedEnvelope)(input.event).data,
  );
  const receipt = await input.step.run("record-and-claim-delivery", () =>
    runEffect(
      recordAndClaimGithubDelivery({
        deliveryId: payload.deliveryId,
        eventKind: "check_suite",
        installationId: payload.installationId,
        repositoryId: payload.repositoryId,
        checkSuiteId: payload.checkSuiteId,
        checkSuiteAction: payload.action,
        headSha: payload.headSha,
        checkSuiteStatus: payload.status,
        checkSuiteConclusion: payload.conclusion,
        processingRunId: input.runId,
      }),
    ),
  );
  if (receipt.disposition === "terminal") {
    await drainGithubCheckSuiteTransitionOutbox(
      input.step,
      "check-suite-replay",
      runEffect,
    );
    return receipt;
  }
  if (receipt.disposition === "owned_elsewhere") return receipt;
  const repository = await input.step.run("resolve-repository", () =>
    runEffect(
      resolveInstallationRepository(
        payload.installationId,
        payload.repositoryId,
      ),
    ),
  );
  const testimony = await input.step.run("refetch-check-suite", async () => {
    const fetched = await runEffect(
      fetchInstallationCheckSuite(
        payload.installationId,
        repository,
        payload.checkSuiteId,
      ),
    );
    if (fetched.headSha !== payload.headSha) {
      throw new NonRetriableError("GitHub check-suite head SHA changed.");
    }
    return fetched;
  });

  const applyTestimony = (stepName: string) =>
    input.step.run(stepName, () =>
      runEffect(
        applyGithubCheckSuiteTestimony({
          deliveryId: payload.deliveryId,
          receiptSequence: receipt.receiptSequence,
          processingRunId: input.runId,
          installationId: payload.installationId,
          repositoryId: payload.repositoryId,
          checkSuiteId: payload.checkSuiteId,
          headSha: testimony.headSha,
          status: testimony.status,
          conclusion: testimony.conclusion,
          sourceUpdatedAt: new Date(testimony.updatedAt),
        }).pipe(
          Effect.map(
            (value): CheckSuiteApplyOutcome => ({ kind: "applied", value }),
          ),
          Effect.catchIf(isPendingPublication, () =>
            Effect.succeed({ kind: "pending_publication" as const }),
          ),
        ),
      ),
    );
  const applied = await applyTestimony("apply-check-suite-testimony");
  if (applied.kind === "applied") {
    await drainGithubCheckSuiteTransitionOutbox(
      input.step,
      "check-suite",
      runEffect,
    );
    return applied.value;
  }
  await publishPendingGithubCheckSuiteTransition(
    input.step,
    "check-suite-predecessor",
    {
      installationId: payload.installationId,
      repositoryId: payload.repositoryId,
      checkSuiteId: payload.checkSuiteId,
    },
    runEffect,
  );
  const finalApplied = await applyTestimony(
    "apply-check-suite-testimony-after-publication",
  );
  if (finalApplied.kind === "applied") {
    await drainGithubCheckSuiteTransitionOutbox(
      input.step,
      "check-suite",
      runEffect,
    );
    return finalApplied.value;
  }
  throw new Error("GitHub check-suite testimony stayed unpublished.");
}

export const createProcessGithubPushReceived = (inngest: PloyzInngest) =>
  inngest.createFunction(
  {
    id: PROCESS_GITHUB_PUSH_RECEIVED_FUNCTION_ID,
    retries: 5,
    triggers: [{ event: githubPushReceivedEventType }],
    concurrency: [...GITHUB_PUSH_RECEIVED_CONCURRENCY],
    onFailure: async ({ event }) =>
      failGithubIngestionDeliveryOnRetryExhausted(event, runInngestEffect),
  },
  async ({ event, step, runId }) =>
    executeProcessGithubPushReceived(
      { event, step, runId },
      runInngestEffect,
    ),
  );

export const createProcessGithubCheckSuiteReceived = (inngest: PloyzInngest) =>
  inngest.createFunction(
  {
    id: PROCESS_GITHUB_CHECK_SUITE_RECEIVED_FUNCTION_ID,
    retries: 5,
    triggers: [{ event: githubCheckSuiteReceivedEventType }],
    concurrency: [...GITHUB_CHECK_SUITE_RECEIVED_CONCURRENCY],
    onFailure: async ({ event }) =>
      failGithubIngestionDeliveryOnRetryExhausted(event, runInngestEffect),
  },
  async ({ event, step, runId }) =>
    executeProcessGithubCheckSuiteReceived(
      { event, step, runId },
      runInngestEffect,
    ),
  );
