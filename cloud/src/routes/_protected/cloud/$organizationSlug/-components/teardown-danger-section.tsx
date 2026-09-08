"use client";

import { useEffect, useState } from "react";
import { Trash2Icon } from "lucide-react";
import { toast } from "sonner";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useServerFn } from "@tanstack/react-start";
import { TeardownDataLossDialog } from "#/components/data-loss/data-loss-confirm-dialog";
import {
  Alert,
  AlertAction,
  AlertDescription,
  AlertTitle,
} from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { Spinner } from "#/components/ui/spinner";
import {
  confirmTeardownServerFn,
  loadLatestTeardownAttemptServerFn,
  loadTeardownDataLossServerFn,
  retryTeardownServerFn,
} from "#/modules/runtime/teardown.functions";
import { latestTeardownAttemptQueryOptions } from "#/modules/runtime/teardown.queries";
import {
  teardownCompletedDescription,
  teardownIsBusy,
  teardownIsRetryable,
  type TeardownAttemptStatus,
  type TeardownOutcome,
  type TeardownScope,
} from "#/modules/runtime/teardown";
import { useRuntimeStatus } from "#/providers/runtime-provider";

type TeardownAttemptSummary = {
  id: string;
  status: TeardownAttemptStatus;
  failureMessage: string | null;
  outcome: TeardownOutcome | null;
};

export function TeardownDangerSection({
  organizationSlug,
  scope,
  environmentId,
  projectSlug,
  confirmPhrase,
  title,
  description,
  actionLabel,
  headingId,
  showHeading = true,
  onCompleted,
}: {
  organizationSlug: string;
  scope: TeardownScope;
  environmentId?: string;
  projectSlug?: string;
  confirmPhrase: string;
  title: string;
  description: string;
  actionLabel: string;
  headingId: string;
  showHeading?: boolean;
  onCompleted?: () => void;
}) {
  const loadDataLoss = useServerFn(loadTeardownDataLossServerFn);
  const confirmTeardown = useServerFn(confirmTeardownServerFn);
  const retryTeardown = useServerFn(retryTeardownServerFn);
  const loadLatest = useServerFn(loadLatestTeardownAttemptServerFn);
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [retrying, setRetrying] = useState(false);
  const input = {
    organizationSlug,
    scope,
    environmentId,
    projectSlug,
  };
  const latestQuery = latestTeardownAttemptQueryOptions(input, () =>
    loadLatest({ data: input }),
  );
  const latest = useQuery(latestQuery);
  const attempt = latest.data ?? null;
  const busy = attempt != null && teardownIsBusy(attempt.status);
  const { lensStatus } = useRuntimeStatus();
  const abandon = scope === "organization" && lensStatus === "unreachable";
  const resolvedTitle = abandon
    ? "Abandon this organization's cluster"
    : title;
  const resolvedDescription = abandon
    ? "Can't reach the cluster. This drops Cloud management and pairing without verifying runtime removal. Runtime membership stays unknown."
    : description;
  const resolvedActionLabel = abandon ? "Abandon cluster" : actionLabel;

  useEffect(() => {
    if (attempt?.status === "completed") onCompleted?.();
  }, [attempt?.status]);

  function remember(next: NonNullable<typeof latest.data>) {
    queryClient.setQueryData(latestQuery.queryKey, next);
  }

  async function handleRetry() {
    if (!attempt || retrying) return;
    setRetrying(true);
    try {
      const next = await retryTeardown({
        data: { organizationSlug, attemptId: attempt.id },
      });
      remember(next);
      toast.success("Teardown retry started.");
    } catch (error) {
      toast.error(
        error instanceof Error
          ? error.message
          : "The teardown couldn’t be retried.",
      );
    } finally {
      setRetrying(false);
    }
  }

  return (
    <>
      <section aria-labelledby={headingId}>
        {showHeading ? (
          <h2 id={headingId} className="text-lg font-semibold text-destructive">
            Danger
          </h2>
        ) : (
          <h2 id={headingId} className="sr-only">
            {title}
          </h2>
        )}
        <div className={showHeading ? "mt-4 flex flex-col gap-4" : "flex flex-col gap-4"}>
          {attempt ? (
            <TeardownStatusAlert
              attempt={attempt}
              retrying={retrying}
              onRetry={() => {
                void handleRetry();
              }}
            />
          ) : null}
          <div className="flex flex-col items-start justify-between gap-4 rounded-xl border border-destructive-border bg-destructive-soft p-4 sm:flex-row sm:items-center">
            <div className="min-w-0">
              <div className="text-sm font-semibold text-destructive">
                {resolvedTitle}
              </div>
              <p className="mt-1 text-sm text-destructive/85">
                {resolvedDescription}
              </p>
            </div>
            <Button
              variant="destructive"
              className="shrink-0"
              disabled={busy}
              onClick={() => setOpen(true)}
            >
              <Trash2Icon data-icon="inline-start" />
              {resolvedActionLabel}
            </Button>
          </div>
        </div>
      </section>
      <TeardownDataLossDialog
        open={open}
        onOpenChange={setOpen}
        confirmPhrase={confirmPhrase}
        callbacks={{
          load: () => loadDataLoss({ data: input }),
          confirm: async (identities) => {
            const next = await confirmTeardown({
              data: { ...input, identities, abandon },
            });
            remember(next);
            toast.success(abandon ? "Abandon started." : "Teardown started.");
          },
        }}
      />
    </>
  );
}

function teardownStatusCopy(attempt: TeardownAttemptSummary) {
  switch (attempt.status) {
    case "pending":
    case "running":
      return {
        title: "Tearing down",
        description: "Inngest is destroying confirmed Data Loss, then Cloud rows.",
      };
    case "partial":
      return {
        title: "Teardown is incomplete",
        description:
          "Review fresh Data Loss and confirm a new teardown to finish the remaining work.",
      };
    case "cancelled":
      return {
        title: "Teardown cancelled",
        description:
          attempt.failureMessage ??
          "Review fresh Data Loss and confirm a new teardown.",
      };
    case "failed":
      return {
        title: "Teardown failed",
        description:
          attempt.failureMessage ??
          "Review fresh Data Loss and confirm a new teardown.",
      };
    case "completed":
      if (attempt.outcome === null) {
        throw new Error("Completed teardown is missing its runtime outcome.");
      }
      return {
        title: "Teardown finished",
        description: teardownCompletedDescription(attempt.outcome),
      };
    default: {
      const exhaustive: never = attempt.status;
      return exhaustive;
    }
  }
}

function TeardownStatusAlert({
  attempt,
  retrying,
  onRetry,
}: {
  attempt: TeardownAttemptSummary;
  retrying: boolean;
  onRetry: () => void;
}) {
  const retryable = teardownIsRetryable(attempt.status);
  const { title, description } = teardownStatusCopy(attempt);

  return (
    <Alert variant={retryable ? "destructive" : "default"}>
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription>{description}</AlertDescription>
      {retryable ? (
        <AlertAction>
          <Button
            variant="outline"
            size="sm"
            disabled={retrying}
            onClick={onRetry}
          >
            {retrying ? <Spinner data-icon="inline-start" /> : null}
            Retry
          </Button>
        </AlertAction>
      ) : null}
    </Alert>
  );
}
