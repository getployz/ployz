import { useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { Trash2Icon } from "lucide-react";
import { toast } from "sonner";
import {
  MachineRemoveDataLossDialog,
  type DataLossConfirmResult,
} from "#/components/data-loss/data-loss-confirm-dialog";
import { Button } from "#/components/ui/button";
import {
  enqueueMachineRemoveServerFn,
  getMachineRemoveAttemptServerFn,
  loadMachineDataLossServerFn,
} from "#/modules/machines/machine-removal.functions";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { DangerRow } from "#/routes/_protected/cloud/$organizationSlug/-components/danger-row";

/** Polls the removal until it settles. Closing the dialog aborts; polling stops within a second. */
async function waitForMachineRemoveAttempt(
  organizationSlug: string,
  attemptId: string,
  signal: AbortSignal,
): Promise<"removed" | "aborted" | DataLossConfirmResult> {
  for (;;) {
    if (signal.aborted) return "aborted";
    const attempt = await getMachineRemoveAttemptServerFn({
      data: { organizationSlug, attemptId },
    });
    if (signal.aborted) return "aborted";
    switch (attempt.state) {
      case "pending":
      case "running":
        await new Promise((resolve) => setTimeout(resolve, 1_000));
        continue;
      case "succeeded":
        return "removed";
      case "missing_identities":
        return {
          state: "missing_identities",
          identities: attempt.missingIdentities,
        };
      case "failed":
      case "cancelled":
        throw new Error(attempt.failureMessage);
      default: {
        const _exhaustive: never = attempt;
        throw new Error(`Unhandled machine remove attempt: ${_exhaustive}`);
      }
    }
  }
}

/** Removing a Server is destructive and waits on the runtime; once it is gone, the page returns to Servers. */
export function RemoveServerSection({ machine, organizationSlug }: { machine: RuntimeMachineRecord; organizationSlug: string }) {
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);
  const abortRef = useRef<AbortController | null>(null);

  function onOpenChange(next: boolean) {
    if (!next) abortRef.current?.abort();
    setOpen(next);
  }

  return (
    <section aria-labelledby="remove-server-heading">
      <h2 id="remove-server-heading" className="sr-only">Remove server</h2>
      <DangerRow
        title={`Remove ${machine.name}`}
        description="Deletes the data stored on it and resets it. Services that run only here stop."
        action={
          <Button variant="destructive" className="shrink-0" onClick={() => onOpenChange(true)}>
            <Trash2Icon data-icon="inline-start" />
            Remove server
          </Button>
        }
      />
      <MachineRemoveDataLossDialog
        open={open}
        onOpenChange={onOpenChange}
        confirmPhrase={machine.name}
        callbacks={{
          load: () =>
            loadMachineDataLossServerFn({
              data: { organizationSlug, machineId: machine.id },
            }),
          confirm: async (rust) => {
            abortRef.current?.abort();
            const abort = new AbortController();
            abortRef.current = abort;
            const queued = await enqueueMachineRemoveServerFn({
              data: { organizationSlug, machineId: machine.id, confirmDataLoss: rust },
            });
            const result = await waitForMachineRemoveAttempt(organizationSlug, queued.id, abort.signal);
            if (result === "removed") {
              toast.success(`${machine.name} removed`);
              void navigate({ to: "/cloud/$organizationSlug/~/servers", params: { organizationSlug } });
            }
            return result === "removed" || result === "aborted" ? undefined : result;
          },
        }}
      />
    </section>
  );
}
