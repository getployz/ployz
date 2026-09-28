"use client";

import { useEffect, useState } from "react";
import { Trash2Icon } from "lucide-react";
import { toast } from "sonner";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useServerFn } from "@tanstack/react-start";
import { DeletionDialog, type DeletionItem } from "#/components/deletion-dialog";
import { Alert, AlertAction, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { Item, ItemActions, ItemContent, ItemDescription, ItemTitle } from "#/components/ui/item";
import { Spinner } from "#/components/ui/spinner";
import { isNotFound, toErrorMessage } from "#/lib/error-message";
import type { DataLossList } from "#/modules/runtime/data-loss-confirm";
import {
  confirmTeardownServerFn,
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
import { DangerRow } from "./danger-row";

type TeardownAttemptSummary = {
  id: string;
  status: TeardownAttemptStatus;
  failureMessage: string | null;
  outcome: TeardownOutcome | null;
};

/**
 * Deletes an Environment, project or organization, or closes a Kept Branch: the roots of real data, so the dialog lists
 * what goes and asks for `place`. Branches that aren't kept close with one plain confirm from their panel instead.
 * `inline` makes it one quiet line with no Danger heading, for the Branch's panel: the typed dialog is the guard.
 */
export function TeardownDangerSection({
  inline = false,
  organizationSlug,
  scope,
  environmentId,
  projectSlug,
  name,
  place,
  verb = "Delete",
  title,
  description,
  actionLabel,
  items,
  disabledReason,
  headingId,
  onCompleted,
}: {
  inline?: boolean;
  organizationSlug: string;
  scope: TeardownScope;
  environmentId?: string;
  projectSlug?: string;
  /** What goes, as the dialog and status name it: "staging". */
  name: string;
  /** Where it is, typed to confirm: "shop/staging". */
  place: string;
  verb?: "Delete" | "Close";
  title: string;
  description: string;
  actionLabel: string;
  /** What Cloud knows goes with it; the servers add any volume Cloud doesn't name. */
  items: readonly DeletionItem[];
  /** Why it can't start, which disables it. */
  disabledReason?: string;
  headingId: string;
  onCompleted?: () => void;
}) {
  const loadDataLoss = useServerFn(loadTeardownDataLossServerFn);
  const confirmTeardown = useServerFn(confirmTeardownServerFn);
  const retryTeardown = useServerFn(retryTeardownServerFn);
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [retrying, setRetrying] = useState(false);
  const input = { organizationSlug, scope, environmentId, projectSlug };
  const latestQuery = latestTeardownAttemptQueryOptions(input);
  const latest = useQuery(latestQuery);
  const attempt = latest.data ?? null;
  const busy = attempt != null && teardownIsBusy(attempt.status);
  const { lensStatus } = useRuntimeStatus();
  // An organization whose servers can't be reached can still go: Ployz lets go of them without resetting them.
  const abandon = scope === "organization" && lensStatus === "unreachable";
  const doing = `${abandon ? "Abandoning" : verb === "Close" ? "Closing" : "Deleting"} ${name}`;

  // Deleting drops the rows its status is read through, so it's done once the read can't find them.
  const gone = busy && isNotFound(latest.error);
  useEffect(() => {
    if (gone) onCompleted?.();
  }, [gone]);

  async function handleRetry() {
    if (!attempt || retrying) return;
    setRetrying(true);
    try {
      queryClient.setQueryData(latestQuery.queryKey, await retryTeardown({ data: { organizationSlug, attemptId: attempt.id } }));
    } catch (error) {
      toast.error(toErrorMessage(error, "Couldn't try again."));
    } finally {
      setRetrying(false);
    }
  }

  const status = attempt ? <TeardownStatusAlert attempt={attempt} doing={doing} retrying={retrying} onRetry={() => void handleRetry()} /> : null;
  const dialog = (
    <DeletionDialog
      open={open}
      onOpenChange={setOpen}
      title={abandon ? `Abandon ${name}'s servers?` : `${verb} ${name}?`}
      place={place}
      confirmLabel={abandon ? "Abandon" : verb}
      items={items}
      callbacks={{
        load: async () => {
          const { rust } = await loadDataLoss({ data: input });
          return { items: withServerVolumes(items, rust), evidence: rust };
        },
        confirm: async (identities) => {
          queryClient.setQueryData(latestQuery.queryKey, await confirmTeardown({ data: { ...input, identities, abandon } }));
          toast(doing);
        },
      }}
    />
  );

  if (inline) {
    return (
      <>
        {status}
        <Item size="sm">
          <ItemContent>
            <ItemTitle>{title}</ItemTitle>
            <ItemDescription>{disabledReason ?? description}</ItemDescription>
          </ItemContent>
          <ItemActions>
            <Button size="sm" variant="outline" disabled={busy || disabledReason !== undefined} onClick={() => setOpen(true)}>{actionLabel}</Button>
          </ItemActions>
        </Item>
        {dialog}
      </>
    );
  }

  return (
    <>
      <section aria-labelledby={headingId}>
        <h2 id={headingId} className="text-lg font-semibold text-destructive">
          Danger
        </h2>
        <div className="mt-4 flex flex-col gap-4">
          {status}
          <DangerRow
            title={abandon ? "Abandon this organization's servers" : title}
            description={abandon ? "Can't reach them. This deletes the organization and leaves the servers as they are." : description}
            action={
              <Button variant="destructive" className="shrink-0" disabled={busy || disabledReason !== undefined} onClick={() => setOpen(true)}>
                <Trash2Icon data-icon="inline-start" />
                {abandon ? "Abandon servers" : actionLabel}
              </Button>
            }
          >
            {disabledReason && <p className="mt-1 text-sm text-muted-foreground">{disabledReason}</p>}
          </DangerRow>
        </div>
      </section>
      {dialog}
    </>
  );
}

/** Cloud's list, plus any volume the servers hold that Cloud doesn't name. */
function withServerVolumes(items: readonly DeletionItem[], rust: DataLossList["rust"]): DeletionItem[] {
  const named = new Set(items.flatMap((item) => item.kind === "volume" ? [item.name] : []));
  const extra = [...new Set(rust.map((identity) => identity.id.name))].filter((volume) => !named.has(volume));
  return [...items, ...extra.map((volume): DeletionItem => ({ kind: "volume", name: volume }))];
}

function teardownStatusCopy(attempt: TeardownAttemptSummary, doing: string) {
  switch (attempt.status) {
    case "pending":
    case "running":
      return { title: `${doing}…`, description: undefined };
    case "partial":
      // Partial says the runtime work is incomplete or its outcome unknown, never why: don't guess.
      return { title: `${doing} didn't finish`, description: "Some of it may still be on your servers. Try again to finish." };
    case "cancelled":
      return { title: `${doing} was cancelled`, description: attempt.failureMessage };
    case "failed":
      return { title: `${doing} failed`, description: attempt.failureMessage };
    case "completed":
      if (attempt.outcome === null) {
        throw new Error("Completed teardown is missing its runtime outcome.");
      }
      return { title: "Done", description: teardownCompletedDescription(attempt.outcome) };
    default: {
      const exhaustive: never = attempt.status;
      return exhaustive;
    }
  }
}

function TeardownStatusAlert({ attempt, doing, retrying, onRetry }: {
  attempt: TeardownAttemptSummary;
  doing: string;
  retrying: boolean;
  onRetry: () => void;
}) {
  const retryable = teardownIsRetryable(attempt.status);
  const { title, description } = teardownStatusCopy(attempt, doing);

  return (
    <Alert variant={attempt.status === "failed" || attempt.status === "partial" ? "destructive" : "default"}>
      {teardownIsBusy(attempt.status) ? <Spinner /> : null}
      <AlertTitle>{title}</AlertTitle>
      {description ? <AlertDescription>{description}</AlertDescription> : null}
      {retryable ? (
        <AlertAction>
          <Button variant="outline" size="sm" disabled={retrying} onClick={onRetry}>
            {retrying ? <Spinner data-icon="inline-start" /> : null}
            Retry
          </Button>
        </AlertAction>
      ) : null}
    </Alert>
  );
}
