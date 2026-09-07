"use client";

import { useState } from "react";
import { toast } from "sonner";
import { getDestructiveVolumeAttemptsCollection } from "#/electric/collections";
import { Badge } from "#/components/ui/badge";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "#/components/ui/card";
import { DropdownMenuItem } from "#/components/ui/dropdown-menu";
import {
  destructiveVolumeRetryModeForAttempt,
  type DestructiveVolumeRetryMode,
} from "#/modules/operations/destructive-volume-attempt";
import type {
  DestructiveVolumeReview,
  DestructiveVolumeSubmissionOutcome,
  EnvironmentDeploymentSummary,
} from "#/modules/deployments/deployment-contract";
import {
  prepareDestructiveVolumeRetryServerFn,
  retryDestructiveVolumeAttemptServerFn,
} from "#/modules/deployments/deployment.functions";
import { prepareVolumeDestructionReview } from "#/components/destructive-volume/destructive-volume-review";
import { VolumeDestructionConfirmationDialog } from "#/components/destructive-volume/volume-destruction-confirmation-dialog";

type Attempt =
  EnvironmentDeploymentSummary["destructiveVolumeAttempts"][number];
type PreparedRetry = {
  mode: DestructiveVolumeRetryMode;
  reviews: DestructiveVolumeReview[];
};
type RetrySubmissionResponse =
  | DestructiveVolumeSubmissionOutcome
  | { txid: number; data: DestructiveVolumeSubmissionOutcome };
type BadgeVariant =
  | "secondary"
  | "info"
  | "success"
  | "warning"
  | "destructive";
const byteFormatter = new Intl.NumberFormat("en-US");
const dateTimeFormatter = new Intl.DateTimeFormat("en-US", {
  year: "numeric",
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
  timeZone: "UTC",
  timeZoneName: "short",
});

export function useDestructiveVolumeRetryControl({
  attempts,
  organizationSlug,
  environmentSlug,
  deploymentStatus,
}: {
  attempts: Attempt[];
  organizationSlug: string | null;
  environmentSlug: string;
  deploymentStatus: EnvironmentDeploymentSummary["status"];
}) {
  const [selectedAttemptId, setSelectedAttemptId] = useState<string | null>(
    null,
  );
  const retriedAttemptIds = new Set(
    attempts.flatMap((attempt) =>
      attempt.retryOfAttemptId ? [attempt.retryOfAttemptId] : [],
    ),
  );
  const retryableAttempt = [...attempts]
    .reverse()
    .find(
      (attempt) =>
        destructiveVolumeRetryModeForAttempt(attempt) !== null &&
        !retriedAttemptIds.has(attempt.id),
    );
  const selectedAttempt = selectedAttemptId
    ? attempts.find((attempt) => attempt.id === selectedAttemptId) ?? null
    : null;
  const selectedMode = selectedAttempt
    ? destructiveVolumeRetryModeForAttempt(selectedAttempt)
    : null;

  async function loadRetry() {
    if (!organizationSlug || !selectedAttempt || !selectedMode) {
      throw new Error("No destructive volume retry is selected.");
    }
    const prepared: PreparedRetry = await prepareDestructiveVolumeRetryServerFn({
        data: {
          organizationSlug,
          attemptId: selectedAttempt.id,
        },
      });
    if (prepared.mode !== selectedMode) {
      throw new Error("The safe volume retry action changed. Review it again.");
    }
    return prepareVolumeDestructionReview({
      reviews: prepared.reviews,
      expectedResourceIds: [selectedAttempt.environmentResourceId],
      expectedNamespaceId: environmentSlug,
    });
  }

  async function confirmRetry(
    preparation: Awaited<ReturnType<typeof loadRetry>>,
  ) {
    if (!organizationSlug || !selectedAttempt || !selectedMode) {
      throw new Error("No destructive volume retry is selected.");
    }
    const [review] = preparation.reviews;
    if (!review || preparation.reviews.length !== 1) {
      throw new Error("A destructive volume retry requires one exact review.");
    }
    const result: RetrySubmissionResponse = await retryDestructiveVolumeAttemptServerFn({
        data: {
          organizationSlug,
          attemptId: selectedAttempt.id,
          review,
        },
      });
    const outcome = "data" in result ? result.data : result;
    if (outcome.state === "review_updated_evidence") {
      return {
        state: "review_updated_evidence" as const,
        preparation: prepareVolumeDestructionReview({
          reviews: outcome.freshReviews,
          expectedResourceIds: [selectedAttempt.environmentResourceId],
          expectedNamespaceId: environmentSlug,
        }),
      };
    }
    if ("txid" in result) {
      await getDestructiveVolumeAttemptsCollection(
        organizationSlug,
      ).utils.awaitTxId(result.txid);
    }
    toast.success(successMessage(selectedMode));
    return { state: "submitted" as const };
  }

  const visible = Boolean(
    organizationSlug && retryableAttempt && deploymentStatus === "applied",
  );
  const actionLabel = retryActionLabel(
    retryableAttempt
      ? destructiveVolumeRetryModeForAttempt(retryableAttempt)
      : null,
  );
  return {
    menuItem: visible && retryableAttempt ? (
      <DropdownMenuItem
        onClick={() => setSelectedAttemptId(retryableAttempt.id)}
      >
        {actionLabel}
      </DropdownMenuItem>
    ) : null,
    dialog: selectedAttempt && selectedMode ? (
      <VolumeDestructionConfirmationDialog
        open
        onOpenChange={(open) => {
          if (!open) setSelectedAttemptId(null);
        }}
        confirmPhrase={environmentSlug}
        actionLabel={retryActionLabel(selectedMode)}
        pendingActionLabel="Working..."
        callbacks={{ load: loadRetry, confirm: confirmRetry }}
      />
    ) : null,
  };
}

export function DestructiveVolumeAttemptHistory({
  attempts,
}: {
  attempts: Attempt[];
}) {
  return (
    <section
      aria-label="Volume removal history"
      className="flex flex-col gap-2 py-2"
    >
      <strong>Volume removal evidence</strong>
      {attempts.map((attempt) => {
        const availability = attempt.evidence.evidence.availability;
        return (
          <Card key={attempt.id}>
            <CardHeader>
              <CardTitle className="flex flex-wrap items-center gap-2">
                <Badge variant={destructiveVolumeBadge(attempt.disposition)}>
                  {attempt.disposition.replaceAll("_", " ")}
                </Badge>
                <span className="font-mono">{attempt.target.volumeName}</span>
              </CardTitle>
              <CardDescription>
                pinned to {attempt.target.machineId} ·{" "}
                {availability.status === "available"
                  ? `${byteFormatter.format(availability.usedBytes)} bytes used · last write ${dateTimeFormatter.format(new Date(availability.lastWriteUnixSeconds * 1_000))}`
                  : `${availability.status === "no_answer" ? "No answer" : "Unavailable"} · size and recency unknown`}
              </CardDescription>
            </CardHeader>
            {attempt.operationId || attempt.failure ? (
              <CardContent className="flex flex-col gap-1">
                {attempt.operationId ? (
                  <p className="break-all font-mono text-xs text-muted-foreground">
                    Core operation {attempt.operationId}
                  </p>
                ) : null}
                {attempt.failure ? (
                  <p className="text-destructive">{attempt.failure.message}</p>
                ) : null}
              </CardContent>
            ) : null}
          </Card>
        );
      })}
    </section>
  );
}

function retryActionLabel(mode: DestructiveVolumeRetryMode | null) {
  switch (mode) {
    case "new_removal":
      return "Retry volume removal";
    case "reobserve_operation":
      return "Resume volume observation";
    case "reconcile_tombstone":
      return "Retry Dashboard cleanup";
    case null:
      return "Retry volume removal";
  }
}

function successMessage(mode: DestructiveVolumeRetryMode) {
  switch (mode) {
    case "new_removal":
      return "Volume removal retry queued.";
    case "reobserve_operation":
      return "Volume observation resumed.";
    case "reconcile_tombstone":
      return "Dashboard volume cleanup completed.";
  }
}

function destructiveVolumeBadge(
  disposition: Attempt["disposition"],
): BadgeVariant {
  if (disposition === "completed") return "success";
  if (disposition === "active" || disposition === "accepted") return "info";
  if (disposition === "partial" || disposition === "cloud_timeout") {
    return "warning";
  }
  return "destructive";
}
