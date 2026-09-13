import { and, eq, sql } from "drizzle-orm";
import { Effect } from "effect";
import type {
  GithubWebhookFailureCode,
  GithubWebhookOutcome,
  GithubWebhookProcessingState,
} from "#/modules/github/tables";
import { asString } from "#/lib/json";
import {
  GITHUB_CHECK_SUITE_ACTIONS,
  GITHUB_CHECK_SUITE_CONCLUSIONS,
  GITHUB_CHECK_SUITE_STATUSES,
  isValidGithubBranchRef,
  isValidGithubExactSha,
  isValidGithubId,
} from "#/modules/github/github-ingestion.contracts";
import { withGithubTransaction } from "#/modules/github/github-ingestion.transaction";
import {
  type GithubDeliveryAdmissionInput,
  type GithubDeliveryFailureCode,
  type GithubDeliveryInput,
  type GithubDeliveryLifecycleResult,
  type GithubDeliveryReceipt,
  type GithubDeliveryTerminalEvidence,
  type GithubMalformedDeliveryInput,
  repositoryError,
} from "#/modules/github/github-ingestion.repository.types";
import { Database } from "#/server/database.server";
import { githubWebhookDelivery as schemaGithubWebhookDelivery } from "#/modules/github/tables";

type DeliveryRow = typeof schemaGithubWebhookDelivery.$inferSelect;

type NormalizedDelivery = {
  deliveryId: string;
  eventKind: DeliveryRow["eventKind"];
  installationId: number;
  repositoryId: number;
  ref: string | null;
  branchState: DeliveryRow["branchState"];
  headSha: string | null;
  checkSuiteId: number | null;
  checkSuiteAction: DeliveryRow["checkSuiteAction"];
  checkSuiteStatus: DeliveryRow["checkSuiteStatus"];
  checkSuiteConclusion: DeliveryRow["checkSuiteConclusion"];
};

function isCanonicalDeliveryId<T>(value: T): value is T & string {
  const text = asString(value);
  return text !== null && /^[A-Za-z0-9][A-Za-z0-9._:-]{0,254}$/.test(text);
}

function isCanonicalRunId<T>(value: T): value is T & string {
  const text = asString(value);
  return (
    text !== null &&
    text.length <= 255 &&
    /^[A-Za-z0-9][A-Za-z0-9._:-]*$/.test(text)
  );
}

function normalizeDelivery(input: GithubDeliveryInput): NormalizedDelivery {
  if (input.eventKind === "push") {
    return {
      deliveryId: input.deliveryId,
      eventKind: input.eventKind,
      installationId: input.installationId,
      repositoryId: input.repositoryId,
      ref: input.ref,
      branchState: input.branch.state,
      headSha: input.branch.state === "active" ? input.branch.headSha : null,
      checkSuiteId: null,
      checkSuiteAction: null,
      checkSuiteStatus: null,
      checkSuiteConclusion: null,
    };
  }
  return {
    deliveryId: input.deliveryId,
    eventKind: input.eventKind,
    installationId: input.installationId,
    repositoryId: input.repositoryId,
    ref: null,
    branchState: null,
    headSha: input.headSha,
    checkSuiteId: input.checkSuiteId,
    checkSuiteAction: input.checkSuiteAction,
    checkSuiteStatus: input.checkSuiteStatus,
    checkSuiteConclusion: input.checkSuiteConclusion,
  };
}

function isDeliveryInput(input: GithubDeliveryInput): boolean {
  if (
    !isCanonicalDeliveryId(input.deliveryId) ||
    !isValidGithubId(input.installationId) ||
    !isValidGithubId(input.repositoryId)
  ) {
    return false;
  }
  if (input.eventKind === "push") {
    if (!isValidGithubBranchRef(input.ref)) return false;
    return input.branch.state === "deleted"
      ? !("headSha" in input.branch)
      : isValidGithubExactSha(input.branch.headSha);
  }
  return (
    input.eventKind === "check_suite" &&
    isValidGithubId(input.checkSuiteId) &&
    isValidGithubExactSha(input.headSha) &&
    GITHUB_CHECK_SUITE_ACTIONS.includes(input.checkSuiteAction) &&
    GITHUB_CHECK_SUITE_STATUSES.includes(input.checkSuiteStatus) &&
    (input.checkSuiteConclusion === null ||
      GITHUB_CHECK_SUITE_CONCLUSIONS.includes(input.checkSuiteConclusion))
  );
}

function deliveryMatches(row: DeliveryRow, input: NormalizedDelivery): boolean {
  return (
    row.deliveryId === input.deliveryId &&
    row.eventKind === input.eventKind &&
    row.installationId === input.installationId &&
    row.repositoryId === input.repositoryId &&
    row.ref === input.ref &&
    row.branchState === input.branchState &&
    row.headSha === input.headSha &&
    row.checkSuiteId === input.checkSuiteId &&
    row.checkSuiteAction === input.checkSuiteAction &&
    row.checkSuiteStatus === input.checkSuiteStatus &&
    row.checkSuiteConclusion === input.checkSuiteConclusion
  );
}

function processedEvidence(
  outcome: GithubWebhookOutcome | null,
): GithubDeliveryTerminalEvidence | null {
  switch (outcome) {
    case "branch_projected":
    case "branch_deleted":
    case "branch_rebased_all_services":
    case "check_suite_projected":
    case "check_suite_unchanged":
    case "ignored_stale":
    case "ignored_unconfigured_repository":
    case "ignored_no_matching_service":
      return { state: "processed", outcome };
    case "malformed":
    case "identity_unresolved":
    case "unsupported_action":
    case "processing_failed":
    case "cancelled":
    case null:
      return null;
    default:
      outcome satisfies never;
      return null;
  }
}

function rejectedEvidence(
  outcome: GithubWebhookOutcome | null,
  failureCode: GithubWebhookFailureCode | null,
): GithubDeliveryTerminalEvidence | null {
  switch (failureCode) {
    case "malformed_payload":
      return outcome === "malformed"
        ? { state: "rejected", outcome, failureCode }
        : null;
    case "identity_unresolved":
      return outcome === "identity_unresolved"
        ? { state: "rejected", outcome, failureCode }
        : null;
    case "unsupported_action":
      return outcome === "unsupported_action"
        ? { state: "rejected", outcome, failureCode }
        : null;
    case "observation_failed":
    case "persistence_failed":
    case "publication_failed":
    case "retry_exhausted":
    case "unexpected_error":
    case "inngest_cancelled":
    case null:
      return null;
    default:
      failureCode satisfies never;
      return null;
  }
}

function failedEvidence(
  outcome: GithubWebhookOutcome | null,
  failureCode: GithubWebhookFailureCode | null,
): GithubDeliveryTerminalEvidence | null {
  if (outcome !== "processing_failed") return null;
  switch (failureCode) {
    case "observation_failed":
    case "persistence_failed":
    case "publication_failed":
    case "retry_exhausted":
    case "unexpected_error":
      return { state: "failed", outcome, failureCode };
    case "malformed_payload":
    case "identity_unresolved":
    case "unsupported_action":
    case "inngest_cancelled":
    case null:
      return null;
    default:
      failureCode satisfies never;
      return null;
  }
}

function cancelledEvidence(
  outcome: GithubWebhookOutcome | null,
  failureCode: GithubWebhookFailureCode | null,
): GithubDeliveryTerminalEvidence | null {
  switch (failureCode) {
    case "inngest_cancelled":
      return outcome === "cancelled"
        ? { state: "cancelled", outcome, failureCode }
        : null;
    case "malformed_payload":
    case "identity_unresolved":
    case "unsupported_action":
    case "observation_failed":
    case "persistence_failed":
    case "publication_failed":
    case "retry_exhausted":
    case "unexpected_error":
    case null:
      return null;
    default:
      failureCode satisfies never;
      return null;
  }
}

function terminalEvidence(
  row: DeliveryRow,
): GithubDeliveryTerminalEvidence | null {
  const processingState: GithubWebhookProcessingState = row.processingState;
  switch (processingState) {
    case "received":
    case "processing":
      return null;
    case "processed":
      return processedEvidence(row.outcome);
    case "rejected":
      return rejectedEvidence(row.outcome, row.failureCode);
    case "failed":
      return failedEvidence(row.outcome, row.failureCode);
    case "cancelled":
      return cancelledEvidence(row.outcome, row.failureCode);
    default:
      processingState satisfies never;
      return null;
  }
}

function receiptFor(
  row: DeliveryRow,
  inserted: boolean,
): GithubDeliveryReceipt | null {
  if (row.processingState === "received") {
    return inserted
      ? {
          disposition: "recorded",
          receiptSequence: row.receiptSequence,
          state: "received",
        }
      : {
          disposition: "replayed",
          receiptSequence: row.receiptSequence,
          state: "received",
          processingRunId: null,
        };
  }
  if (row.processingState === "processing") {
    return {
      disposition: "replayed",
      receiptSequence: row.receiptSequence,
      state: "processing",
      processingRunId: row.processingRunId,
    };
  }
  const evidence = terminalEvidence(row);
  return evidence
    ? {
        disposition: "terminal",
        receiptSequence: row.receiptSequence,
        evidence,
      }
    : null;
}

const lockDelivery = Effect.fn("Github.lockDelivery")(function* (
  deliveryId: string,
) {
  const { drizzle } = yield* Database;
  yield* drizzle.execute(
    sql`select pg_advisory_xact_lock(hashtextextended(${deliveryId}, 109))`,
  );
});

export const recordGithubDelivery = Effect.fn("Github.recordDelivery")(
  function* (input: GithubDeliveryInput) {
    if (!isDeliveryInput(input)) {
      return yield* repositoryError("invalid_input", false);
    }
    const normalized = normalizeDelivery(input);
    return yield* withGithubTransaction(
      Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* lockDelivery(normalized.deliveryId);
      const [existing] = yield* drizzle
        .select()
        .from(schemaGithubWebhookDelivery)
        .where(eq(schemaGithubWebhookDelivery.deliveryId, normalized.deliveryId))
        .limit(1);
      if (existing) {
        if (!deliveryMatches(existing, normalized)) {
          return yield* repositoryError("delivery_conflict", false);
        }
        const receipt = receiptFor(existing, false);
        if (!receipt) return yield* repositoryError("database_error", false);
        return receipt;
      }
      const [inserted] = yield* drizzle
        .insert(schemaGithubWebhookDelivery)
        .values({
          deliveryId: normalized.deliveryId,
          eventKind: normalized.eventKind,
          installationId: normalized.installationId,
          repositoryId: normalized.repositoryId,
          ref: normalized.ref,
          branchState: normalized.branchState,
          headSha: normalized.headSha,
          checkSuiteId: normalized.checkSuiteId,
          checkSuiteAction: normalized.checkSuiteAction,
          checkSuiteStatus: normalized.checkSuiteStatus,
          checkSuiteConclusion: normalized.checkSuiteConclusion,
        })
        .returning();
      const receipt = inserted ? receiptFor(inserted, true) : null;
      if (!receipt) return yield* repositoryError("database_error", true);
      return receipt;
    }),
    );
  },
);

export const recordAndClaimGithubDelivery = Effect.fn(
  "Github.recordAndClaimDelivery",
)(function* (input: GithubDeliveryAdmissionInput) {
  if (!isDeliveryInput(input) || !isCanonicalRunId(input.processingRunId)) {
    return yield* repositoryError("invalid_input", false);
  }
  const normalized = normalizeDelivery(input);
  return yield* withGithubTransaction(
    Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* lockDelivery(normalized.deliveryId);
      const [existing] = yield* drizzle
        .select()
        .from(schemaGithubWebhookDelivery)
        .where(eq(schemaGithubWebhookDelivery.deliveryId, normalized.deliveryId))
        .for("update");
      if (existing) {
        if (!deliveryMatches(existing, normalized)) {
          return yield* repositoryError("delivery_conflict", false);
        }
        const evidence = terminalEvidence(existing);
        if (evidence) {
          return {
            disposition: "terminal" as const,
            receiptSequence: existing.receiptSequence,
            evidence,
          };
        }
        if (existing.processingState === "processing") {
          return existing.processingRunId === input.processingRunId
            ? {
                disposition: "resumed" as const,
                receiptSequence: existing.receiptSequence,
              }
            : {
                disposition: "owned_elsewhere" as const,
                receiptSequence: existing.receiptSequence,
                runId: existing.processingRunId ?? "unknown",
              };
        }
        if (existing.processingState !== "received") {
          return yield* repositoryError("database_error", false);
        }
        const [promoted] = yield* drizzle
          .update(schemaGithubWebhookDelivery)
          .set({
            processingState: "processing",
            processingRunId: input.processingRunId,
            processingStartedAt: new Date(),
            updatedAt: new Date(),
          })
          .where(
            and(
              eq(schemaGithubWebhookDelivery.deliveryId, normalized.deliveryId),
              eq(
                schemaGithubWebhookDelivery.receiptSequence,
                existing.receiptSequence,
              ),
              eq(schemaGithubWebhookDelivery.processingState, "received"),
            ),
          )
          .returning({
            receiptSequence: schemaGithubWebhookDelivery.receiptSequence,
          });
        if (!promoted) return yield* repositoryError("run_conflict", true);
        return {
          disposition: "claimed" as const,
          receiptSequence: promoted.receiptSequence,
        };
      }

      const [inserted] = yield* drizzle
        .insert(schemaGithubWebhookDelivery)
        .values({
          deliveryId: normalized.deliveryId,
          eventKind: normalized.eventKind,
          installationId: normalized.installationId,
          repositoryId: normalized.repositoryId,
          ref: normalized.ref,
          branchState: normalized.branchState,
          headSha: normalized.headSha,
          checkSuiteId: normalized.checkSuiteId,
          checkSuiteAction: normalized.checkSuiteAction,
          checkSuiteStatus: normalized.checkSuiteStatus,
          checkSuiteConclusion: normalized.checkSuiteConclusion,
          processingState: "processing",
          processingRunId: input.processingRunId,
          processingStartedAt: new Date(),
        })
        .returning({
          receiptSequence: schemaGithubWebhookDelivery.receiptSequence,
        });
      if (!inserted) return yield* repositoryError("database_error", true);
      return {
        disposition: "claimed" as const,
        receiptSequence: inserted.receiptSequence,
      };
    }),
    );
  },
);

export const claimGithubDelivery = Effect.fn("Github.claimDelivery")(
  function* (input: {
    deliveryId: string;
    receiptSequence: number;
    processingRunId: string;
  }) {
    if (
      !isCanonicalDeliveryId(input.deliveryId) ||
      !isValidGithubId(input.receiptSequence) ||
      !isCanonicalRunId(input.processingRunId)
    ) {
      return yield* repositoryError("invalid_input", false);
    }
    return yield* withGithubTransaction(
      Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* lockDelivery(input.deliveryId);
      const [row] = yield* drizzle
        .select()
        .from(schemaGithubWebhookDelivery)
        .where(eq(schemaGithubWebhookDelivery.deliveryId, input.deliveryId))
        .for("update");
      if (!row || row.receiptSequence !== input.receiptSequence) {
        return yield* repositoryError("delivery_conflict", false);
      }
      const evidence = terminalEvidence(row);
      if (evidence) {
        return {
          disposition: "terminal" as const,
          receiptSequence: row.receiptSequence,
          evidence,
        };
      }
      if (row.processingState === "processing") {
        return row.processingRunId === input.processingRunId
          ? {
              disposition: "resumed" as const,
              receiptSequence: row.receiptSequence,
            }
          : {
              disposition: "owned_elsewhere" as const,
              receiptSequence: row.receiptSequence,
              runId: row.processingRunId ?? "unknown",
            };
      }
      if (row.processingState !== "received") {
        return yield* repositoryError("database_error", false);
      }
      const [claimed] = yield* drizzle
        .update(schemaGithubWebhookDelivery)
        .set({
          processingState: "processing",
          processingRunId: input.processingRunId,
          processingStartedAt: new Date(),
          updatedAt: new Date(),
        })
        .where(
          and(
            eq(schemaGithubWebhookDelivery.deliveryId, input.deliveryId),
            eq(
              schemaGithubWebhookDelivery.receiptSequence,
              input.receiptSequence,
            ),
            eq(schemaGithubWebhookDelivery.processingState, "received"),
          ),
        )
        .returning({
          receiptSequence: schemaGithubWebhookDelivery.receiptSequence,
        });
      if (!claimed) return yield* repositoryError("run_conflict", true);
      return {
        disposition: "claimed" as const,
        receiptSequence: claimed.receiptSequence,
      };
    }),
    );
  },
);

const terminalizeGithubDelivery = Effect.fn("Github.terminalizeDelivery")(
  function* (
    input: { processingRunId: string },
    terminal: {
      processingState: "failed" | "cancelled";
      outcome: "processing_failed" | "cancelled";
      failureCode: GithubDeliveryFailureCode | "inngest_cancelled";
    },
  ) {
    if (!isCanonicalRunId(input.processingRunId)) {
      return yield* repositoryError("invalid_input", false);
    }
    return yield* withGithubTransaction(
      Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const completed = yield* drizzle
        .update(schemaGithubWebhookDelivery)
        .set({ ...terminal, processedAt: new Date(), updatedAt: new Date() })
        .where(
          and(
            eq(
              schemaGithubWebhookDelivery.processingRunId,
              input.processingRunId,
            ),
            eq(schemaGithubWebhookDelivery.processingState, "processing"),
          ),
        )
        .returning({ deliveryId: schemaGithubWebhookDelivery.deliveryId });
      if (completed.length === 1) {
        return { disposition: "terminal" } satisfies GithubDeliveryLifecycleResult;
      }
      if (completed.length > 1) {
        return yield* repositoryError("database_error", false);
      }

      const [existing] = yield* drizzle
        .select({
          processingState: schemaGithubWebhookDelivery.processingState,
          outcome: schemaGithubWebhookDelivery.outcome,
          failureCode: schemaGithubWebhookDelivery.failureCode,
        })
        .from(schemaGithubWebhookDelivery)
        .where(
          eq(
            schemaGithubWebhookDelivery.processingRunId,
            input.processingRunId,
          ),
        )
        .limit(1);
      return existing?.processingState === terminal.processingState &&
        existing.outcome === terminal.outcome &&
        existing.failureCode === terminal.failureCode
        ? { disposition: "terminal" satisfies GithubDeliveryLifecycleResult["disposition"] }
        : { disposition: "not_found" satisfies GithubDeliveryLifecycleResult["disposition"] };
    }),
    );
  },
);

export const failGithubDelivery = Effect.fn("Github.failDelivery")(
  function* (input: {
    processingRunId: string;
    failureCode: GithubDeliveryFailureCode;
  }) {
    return yield* terminalizeGithubDelivery(input, {
      processingState: "failed",
      outcome: "processing_failed",
      failureCode: input.failureCode,
    });
  },
);

export const cancelGithubDelivery = Effect.fn("Github.cancelDelivery")(
  function* (input: { processingRunId: string }) {
    return yield* terminalizeGithubDelivery(input, {
      processingState: "cancelled",
      outcome: "cancelled",
      failureCode: "inngest_cancelled",
    });
  },
);

export const rejectMalformedGithubDelivery = Effect.fn(
  "Github.rejectMalformedDelivery",
)(function* (input: GithubMalformedDeliveryInput) {
  if (
    !isCanonicalDeliveryId(input.deliveryId) ||
    !["push", "check_suite"].includes(input.eventKind) ||
    !["malformed", "unsupported_action"].includes(input.rejection)
  ) {
    return yield* repositoryError("invalid_input", false);
  }
  const outcome =
    input.rejection === "malformed" ? "malformed" : "unsupported_action";
  const failureCode =
    input.rejection === "malformed"
      ? "malformed_payload"
      : "unsupported_action";
  return yield* withGithubTransaction(
    Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* lockDelivery(input.deliveryId);
      const [existing] = yield* drizzle
        .select()
        .from(schemaGithubWebhookDelivery)
        .where(eq(schemaGithubWebhookDelivery.deliveryId, input.deliveryId));
      if (existing) {
        if (
          existing.eventKind !== input.eventKind ||
          existing.processingState !== "rejected" ||
          existing.outcome !== outcome ||
          existing.failureCode !== failureCode
        ) {
          return yield* repositoryError("delivery_conflict", false);
        }
        const receipt = receiptFor(existing, false);
        if (!receipt) return yield* repositoryError("database_error", false);
        return receipt;
      }
      const [inserted] = yield* drizzle
        .insert(schemaGithubWebhookDelivery)
        .values({
          deliveryId: input.deliveryId,
          eventKind: input.eventKind,
          processingState: "rejected",
          outcome,
          failureCode,
          processedAt: new Date(),
        })
        .returning();
      const receipt = inserted ? receiptFor(inserted, true) : null;
      if (!receipt) return yield* repositoryError("database_error", true);
      return receipt;
    }),
    );
  },
);

export const completeGithubDelivery = Effect.fn("Github.completeDelivery")(
  function* (
    input: {
      deliveryId: string;
      receiptSequence: number;
      processingRunId: string;
      identity:
        | {
            eventKind: "push";
            installationId: number;
            repositoryId: number;
            ref: string;
          }
        | {
            eventKind: "check_suite";
            installationId: number;
            repositoryId: number;
            checkSuiteId: number;
          };
    },
    outcome: GithubWebhookOutcome,
  ) {
    const { drizzle } = yield* Database;
    const [completed] = yield* drizzle
      .update(schemaGithubWebhookDelivery)
      .set({
        processingState: "processed",
        outcome,
        processedAt: new Date(),
        updatedAt: new Date(),
      })
      .where(
        and(
          eq(schemaGithubWebhookDelivery.deliveryId, input.deliveryId),
          eq(
            schemaGithubWebhookDelivery.receiptSequence,
            input.receiptSequence,
          ),
          eq(schemaGithubWebhookDelivery.processingState, "processing"),
          eq(
            schemaGithubWebhookDelivery.processingRunId,
            input.processingRunId,
          ),
          eq(schemaGithubWebhookDelivery.eventKind, input.identity.eventKind),
          eq(
            schemaGithubWebhookDelivery.installationId,
            input.identity.installationId,
          ),
          eq(
            schemaGithubWebhookDelivery.repositoryId,
            input.identity.repositoryId,
          ),
          input.identity.eventKind === "push"
            ? eq(schemaGithubWebhookDelivery.ref, input.identity.ref)
            : eq(
                schemaGithubWebhookDelivery.checkSuiteId,
                input.identity.checkSuiteId,
              ),
        ),
      )
      .returning({ deliveryId: schemaGithubWebhookDelivery.deliveryId });
    if (completed) return;

    const [existing] = yield* drizzle
      .select()
      .from(schemaGithubWebhookDelivery)
      .where(eq(schemaGithubWebhookDelivery.deliveryId, input.deliveryId))
      .limit(1);
    const sameIdentity =
      existing?.eventKind === input.identity.eventKind &&
      existing.installationId === input.identity.installationId &&
      existing.repositoryId === input.identity.repositoryId &&
      (input.identity.eventKind === "push"
        ? existing.ref === input.identity.ref
        : existing.checkSuiteId === input.identity.checkSuiteId);
    if (
      !(
        existing?.receiptSequence === input.receiptSequence &&
        existing.processingRunId === input.processingRunId &&
        existing.processingState === "processed" &&
        sameIdentity
      )
    ) {
      return yield* repositoryError("run_conflict", false);
    }
  },
);
