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

function waitMs(ms: number, signal: AbortSignal) {
  return new Promise<void>((resolve, reject) => {
    if (signal.aborted) {
      reject(new DOMException("Aborted", "AbortError"));
      return;
    }
    const timer = setTimeout(resolve, ms);
    signal.addEventListener(
      "abort",
      () => {
        clearTimeout(timer);
        reject(new DOMException("Aborted", "AbortError"));
      },
      { once: true },
    );
  });
}

async function waitForMachineRemoveAttempt(
  organizationSlug: string,
  attemptId: string,
  signal: AbortSignal,
): Promise<void | DataLossConfirmResult> {
  try {
    for (;;) {
      if (signal.aborted) return;
      const attempt = await getMachineRemoveAttemptServerFn({
        data: { organizationSlug, attemptId },
      });
      if (signal.aborted) return;
      switch (attempt.state) {
        case "pending":
        case "running":
          await waitMs(1_000, signal);
          continue;
        case "succeeded":
          return;
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
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") return;
    throw error;
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
      <div className="flex flex-col items-start justify-between gap-4 rounded-xl border border-destructive-border bg-destructive-soft p-4 sm:flex-row sm:items-center">
        <div className="min-w-0">
          <div className="text-sm font-semibold text-foreground">Remove {machine.name}</div>
          <p className="mt-1 text-sm text-foreground">Deletes the data stored on it and resets it. Services that run only here stop.</p>
        </div>
        <Button variant="destructive" className="shrink-0" onClick={() => onOpenChange(true)}>
          <Trash2Icon data-icon="inline-start" />
          Remove server
        </Button>
      </div>
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
            if (result === undefined && !abort.signal.aborted) {
              toast.success(`${machine.name} removed`);
              void navigate({ to: "/cloud/$organizationSlug/~/servers", params: { organizationSlug } });
            }
            return result;
          },
        }}
      />
    </section>
  );
}
