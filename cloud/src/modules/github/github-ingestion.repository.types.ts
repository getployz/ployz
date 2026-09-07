import type { GithubWebhookFailureCode, GithubWebhookOutcome } from "#/modules/github/tables";
import { Data } from "effect";
import type {
  GithubBranchCursor,
  GithubCheckSuiteAction,
  GithubCheckSuiteConclusion,
  GithubCheckSuiteStatus,
  GithubEnvironmentTriggerInput,
  GithubEnvironmentTriggerSelection,
  GithubServiceCandidate,
} from "#/modules/github/github-ingestion.contracts";
import type { BranchEvaluationPlan } from "#/modules/github/github-branch-evaluation";

type GithubPushDeliveryBase = {
  deliveryId: string;
  eventKind: "push";
  installationId: number;
  repositoryId: number;
  ref: string;
};

export type GithubPushDeliveryInput = GithubPushDeliveryBase & {
  branch: { state: "active"; headSha: string } | { state: "deleted" };
};

export type GithubCheckSuiteDeliveryInput = {
  deliveryId: string;
  eventKind: "check_suite";
  installationId: number;
  repositoryId: number;
  checkSuiteId: number;
  checkSuiteAction: GithubCheckSuiteAction;
  headSha: string;
  checkSuiteStatus: GithubCheckSuiteStatus;
  checkSuiteConclusion: GithubCheckSuiteConclusion | null;
};

export type GithubDeliveryInput =
  | GithubPushDeliveryInput
  | GithubCheckSuiteDeliveryInput;

export type GithubDeliveryAdmissionInput = GithubDeliveryInput & {
  processingRunId: string;
};

export type GithubDeliveryTerminalEvidence =
  | {
      state: "processed";
      outcome: Exclude<
        GithubWebhookOutcome,
        | "malformed"
        | "identity_unresolved"
        | "unsupported_action"
        | "processing_failed"
        | "cancelled"
      >;
    }
  | {
      state: "rejected";
      outcome: "malformed" | "identity_unresolved" | "unsupported_action";
      failureCode:
        | "malformed_payload"
        | "identity_unresolved"
        | "unsupported_action";
    }
  | {
      state: "failed";
      outcome: "processing_failed";
      failureCode: Exclude<
        GithubWebhookFailureCode,
        | "malformed_payload"
        | "identity_unresolved"
        | "unsupported_action"
        | "inngest_cancelled"
      >;
    }
  | {
      state: "cancelled";
      outcome: "cancelled";
      failureCode: "inngest_cancelled";
    };

export type GithubDeliveryReceipt =
  | {
      disposition: "recorded";
      receiptSequence: number;
      state: "received";
    }
  | {
      disposition: "replayed";
      receiptSequence: number;
      state: "received" | "processing";
      processingRunId: string | null;
    }
  | {
      disposition: "terminal";
      receiptSequence: number;
      evidence: GithubDeliveryTerminalEvidence;
    };

export type GithubMalformedDeliveryInput = {
  deliveryId: string;
  eventKind: "push" | "check_suite";
  rejection: "malformed" | "unsupported_action";
};

export type GithubDeliveryClaimResult =
  | { disposition: "claimed" | "resumed"; receiptSequence: number }
  | { disposition: "owned_elsewhere"; receiptSequence: number; runId: string }
  | {
      disposition: "terminal";
      receiptSequence: number;
      evidence: GithubDeliveryTerminalEvidence;
    };

export type GithubDeliveryLifecycleResult = {
  disposition: "terminal" | "not_found";
};

export type GithubDeliveryFailureCode = Exclude<
  GithubWebhookFailureCode,
  | "malformed_payload"
  | "identity_unresolved"
  | "unsupported_action"
  | "inngest_cancelled"
>;

export type GithubIngestionRepositoryErrorCode =
  | "delivery_conflict"
  | "cursor_conflict"
  | "run_conflict"
  | "pending_publication"
  | "invalid_input"
  | "invalid_stored_service"
  | "snapshot_not_admitted"
  | "database_error";

export class GithubIngestionRepositoryError extends Data.TaggedError(
  "GithubIngestionRepositoryError",
)<{
  code: GithubIngestionRepositoryErrorCode;
  retriable: boolean;
  message: string;
}> {}

export function repositoryError(
  code: GithubIngestionRepositoryErrorCode,
  retriable: boolean,
): GithubIngestionRepositoryError {
  return new GithubIngestionRepositoryError({
    code,
    retriable,
    message: `GitHub ingestion repository failed (${code}).`,
  });
}

export type GithubBranchIdentity = {
  installationId: number;
  repositoryId: number;
  ref: string;
};

export type GithubBranchEvaluationResult = {
  disposition: "applied" | "stale";
  triggersCreated: number;
  cursor: GithubBranchCursor | null;
};

export type ApplyGithubBranchEvaluationInput = GithubBranchIdentity & {
  deliveryId: string;
  receiptSequence: number;
  processingRunId: string;
  expectedCursor: GithubBranchCursor | null;
  plan: BranchEvaluationPlan;
};

export type ApplyGithubCheckSuiteTestimonyInput = {
  deliveryId: string;
  receiptSequence: number;
  processingRunId: string;
  installationId: number;
  repositoryId: number;
  checkSuiteId: number;
  headSha: string;
  status: GithubCheckSuiteStatus;
  conclusion: GithubCheckSuiteConclusion | null;
  sourceUpdatedAt: Date;
};

export type GithubCheckSuiteTestimonyResult = {
  disposition: "applied" | "unchanged" | "stale";
  transitionRevision: number;
};

export type GithubPendingEnvironmentTrigger = GithubBranchIdentity & {
  triggerId: string;
  headSha: string;
  environmentId: string;
  serviceIds: string[];
  selection: GithubEnvironmentTriggerSelection;
  sourceDeliveryId: string;
  sourceReceiptSequence: number;
  triggerRevision: number;
};

export type GithubPendingCheckSuiteTransition = {
  installationId: number;
  repositoryId: number;
  checkSuiteId: number;
  headSha: string;
  status: GithubCheckSuiteStatus;
  conclusion: GithubCheckSuiteConclusion | null;
  sourceUpdatedAt: Date;
  sourceDeliveryId: string;
  sourceReceiptSequence: number;
  transitionRevision: number;
};

export type {
  GithubBranchCursor,
  GithubEnvironmentTriggerInput,
  GithubServiceCandidate,
};
